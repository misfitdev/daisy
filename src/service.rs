//! Connection orchestration shared by the CLI and native menu-bar app.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, watch};

use crate::clipboard::Sharing;
use crate::discovery::{self, Advertiser};
use crate::identity::{Identity, PublicKey};
use crate::input::Side;
use crate::macos::{self, capture::Capture, inject::Injector, pasteboard::Pasteboard};
use crate::pairing::{PairingPrompt, Trust, establish_trust};
use crate::peers::PeerStore;
use crate::reconnect::{self, KeyChanged, Unreachable};
use crate::session::Channel;
use crate::share;
use crate::trust::{self, Policy};

pub const DEFAULT_PORT: u16 = 24850;

/// How long reaching the peer may take before it counts as unreachable;
/// a sleeping peer otherwise holds a connection attempt for over a minute.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
const TRUST_TIMEOUT: Duration = Duration::from_secs(120);
const PAIRING_WINDOW: Duration = Duration::from_secs(10 * 60);
const MAX_PAIRING_ATTEMPTS: u8 = 5;
const INPUT_QUEUE_CAPACITY: usize = 1024;

type Pairing = Option<Policy>;

/// Every system listens and browses, and holds a link with each trusted
/// peer it finds. Paired keys choose the opener of each link; anonymous
/// pairing beacons use their random nonces for the same election.
pub async fn automatic<P, O>(config: SessionConfig<'_>, prompt: &mut P, observer: &mut O) -> Result<()>
where
    P: PairingPrompt + Clone + Send,
    O: ServiceObserver + Send,
{
    let listener = TcpListener::bind(("0.0.0.0", DEFAULT_PORT)).await?;
    let mut browser = discovery::Browser::start()?;
    let hub = Hub::new(&config);
    let pairing = std::sync::Mutex::new(PairingGate::new(config.pairing));
    let gate = || pairing.lock().unwrap_or_else(|e| e.into_inner());
    let observer = std::sync::Mutex::new(observer);
    let mut discoverable = config.discoverable.clone();
    let mut first_seen: HashMap<[u8; discovery::NONCE_LEN], tokio::time::Instant> = HashMap::new();
    // peers with a link running or being opened
    let busy = std::sync::Mutex::new(std::collections::HashSet::<PublicKey>::new());
    let mut running: Running<'_, Option<Opened>> = Running::default();
    loop {
        if hub.members() == 0 {
            Shared(&observer).waiting(
                config.name,
                config.identity.public_key(),
                DEFAULT_PORT,
                config.pairing.filter(|_| gate().is_open()),
            );
        }
        let advertiser = advertise(&config, DEFAULT_PORT, &gate(), *discoverable.borrow_and_update());
        let mut scan = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                result = listener.accept() => {
                    let (stream, address) = result?;
                    stream.set_nodelay(true)?;
                    let (config, hub, observer, busy, pairing) = (&config, &hub, &observer, &busy, &pairing);
                    let mut prompt = prompt.clone();
                    running.push(async move {
                        let mut shared = Shared(observer);
                        let result = answer(stream, config, hub, pairing, &mut prompt, &mut shared, busy).await;
                        match result {
                            Ok(peer) => shared.disconnected(&peer),
                            Err(error) => shared.connection_failed(&address.to_string(), &error),
                        }
                        None
                    });
                }
                finished = running.next() => {
                    if let Some(opened) = finished {
                        if opened.pairing && opened.key.is_some() { gate().completed = true; }
                        if let Err(error) = opened.result { Shared(&observer).connection_failed(&opened.address, &error); }
                        if let Some(key) = opened.key { busy.lock().unwrap_or_else(|e| e.into_inner()).remove(&key); }
                    }
                    if hub.members() == 0 { break; }
                }
                changed = browser.changed() => {
                    if !changed { anyhow::bail!("Bonjour browsing stopped"); }
                }
                Ok(()) = discoverable.changed() => break,
                _ = scan.tick() => {
                    let closing = { let gate = gate(); gate.policy.is_some() && !gate.is_open() && !gate.closed };
                    if closing {
                        gate().close();
                        Shared(&observer).pairing_closed();
                        break;
                    }
                    if !hub.has_room() { continue; }
                    let keys: Vec<_> = config.peers.list(trust::now())?.iter().map(|p| p.key).collect();
                    let own = config.identity.public_key();
                    let mut found = browser.current(&keys);
                    found.sort_by_key(|heard| heard.election);
                    let now = tokio::time::Instant::now();
                    first_seen.retain(|election, _| found.iter().any(|heard| heard.election == *election));
                    for heard in &found { first_seen.entry(heard.election).or_insert(now); }
                    let election = advertiser.as_ref().map(|a| a.election);
                    let pairing_open = gate().is_open();
                    for heard in found {
                        let seen_for = now - first_seen[&heard.election];
                        if !discovery::opens_connection(own, election, &heard, pairing_open, seen_for) { continue; }
                        let expected = match heard.seen { discovery::Seen::Paired(key) => Some(key), discovery::Seen::Pairing => None };
                        if let Some(key) = expected && !busy.lock().unwrap_or_else(|e| e.into_inner()).insert(key) { continue; }
                        // wait out the grace again, so two systems that both
                        // connected do not keep colliding
                        first_seen.remove(&heard.election);
                        let attempt = SessionConfig { pairing: gate().for_peer(expected.is_some()), ..config };
                        let address = heard.address.to_string();
                        Shared(&observer).connecting(&address, peer_name(config.peers, expected).as_deref());
                        let (hub, observer) = (&hub, &observer);
                        let mut prompt = prompt.clone();
                        running.push(async move {
                            let mut expected = expected;
                            let mut shared = Shared(observer);
                            let (_, result) = connect_once(attempt, hub, &address, &mut expected, &mut prompt, &mut shared).await;
                            Some(Opened { address, key: expected, pairing: heard.seen == discovery::Seen::Pairing, result })
                        });
                    }
                }
            }
        }
    }
}

/// How a link this system opened ended.
struct Opened {
    address: String,
    /// The peer, once known.
    key: Option<PublicKey>,
    /// Whether it began as a pairing.
    pairing: bool,
    result: Result<()>,
}

/// Futures run side by side within one task, so they may borrow from it.
/// Polls each on every wake, which suits the handful a group has.
struct Running<'a, T> {
    futures: Vec<std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send + 'a>>>,
}

impl<T> Default for Running<'_, T> {
    fn default() -> Self {
        Self { futures: Vec::new() }
    }
}

impl<'a, T> Running<'a, T> {
    fn push(&mut self, future: impl std::future::Future<Output = T> + Send + 'a) {
        self.futures.push(Box::pin(future));
    }

    /// The next to finish; never resolves while none are running.
    async fn next(&mut self) -> T {
        std::future::poll_fn(|context| {
            for index in 0..self.futures.len() {
                if let std::task::Poll::Ready(value) = self.futures[index].as_mut().poll(context) {
                    drop(self.futures.swap_remove(index));
                    return std::task::Poll::Ready(value);
                }
            }
            std::task::Poll::Pending
        })
        .await
    }
}

/// One observer shared by links running side by side.
struct Shared<'a, 'b, O>(&'a std::sync::Mutex<&'b mut O>);

impl<O: ServiceObserver> ServiceObserver for Shared<'_, '_, O> {
    fn waiting(&mut self, name: &str, key: PublicKey, port: u16, pairing: Pairing) {
        self.lock().waiting(name, key, port, pairing);
    }
    fn connecting(&mut self, address: &str, peer: Option<&str>) {
        self.lock().connecting(address, peer);
    }
    fn reconnecting(&mut self, address: &str, peer: Option<&str>, wait: Duration) {
        self.lock().reconnecting(address, peer, wait);
    }
    fn paired(&mut self, peer: &str, key: PublicKey, policy: Policy) {
        self.lock().paired(peer, key, policy);
    }
    fn connected(&mut self, peer: &str, key: PublicKey, side: Side) {
        self.lock().connected(peer, key, side);
    }
    fn link(&mut self, peer: &str, link: crate::control::Link) {
        self.lock().link(peer, link);
    }
    fn disconnected(&mut self, peer: &str) {
        self.lock().disconnected(peer);
    }
    fn pairing_closed(&mut self) {
        self.lock().pairing_closed();
    }
    fn connection_failed(&mut self, address: &str, error: &anyhow::Error) {
        self.lock().connection_failed(address, error);
    }
}

impl<'b, O> Shared<'_, 'b, O> {
    fn lock(&self) -> std::sync::MutexGuard<'_, &'b mut O> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Receives human-readable state changes without taking part in protocol logic.
pub trait ServiceObserver {
    fn waiting(&mut self, _name: &str, _key: PublicKey, _port: u16, _pairing: Pairing) {}
    /// `peer` names a paired peer, when it is known which one this is.
    fn connecting(&mut self, _address: &str, _peer: Option<&str>) {}
    /// The connection could not be made or was lost; trying again after `wait`.
    fn reconnecting(&mut self, _address: &str, _peer: Option<&str>, _wait: Duration) {}
    fn paired(&mut self, _peer: &str, _key: PublicKey, _policy: Policy) {}
    fn connected(&mut self, _peer: &str, _key: PublicKey, _side: Side) {}
    /// Latency or who has control changed in the running session.
    fn link(&mut self, _peer: &str, _link: crate::control::Link) {}
    fn disconnected(&mut self, _peer: &str) {}
    fn pairing_closed(&mut self) {}
    fn connection_failed(&mut self, _address: &str, _error: &anyhow::Error) {}
}

/// Observer used by callers that do not render connection state.
#[derive(Debug, Default)]
pub struct SilentObserver;

impl ServiceObserver for SilentObserver {}

/// Stable session inputs shared by listeners and outgoing connections.
#[derive(Clone, Copy)]
pub struct SessionConfig<'a> {
    pub identity: &'a Identity,
    pub peers: &'a PeerStore,
    pub name: &'a str,
    pub pairing: Option<Policy>,
    /// Where a newly paired peer sits.
    pub side: Side,
    /// Also apply `side` to a peer paired before, as a new choice. Taken by
    /// the first session, so a reconnect cannot override a later choice
    /// made on the peer.
    pub choose_side: &'a AtomicBool,
    /// Whether this system shares its clipboard; may change mid-session.
    pub clipboard: &'a watch::Receiver<bool>,
    /// Whether a waiting system advertises itself with Bonjour.
    pub discoverable: &'a watch::Receiver<bool>,
    /// Sides chosen on this system while a session runs, if it can rearrange.
    pub arrangement: Option<&'a watch::Receiver<Option<Side>>>,
}

struct PairingGate {
    policy: Pairing,
    opened: tokio::time::Instant,
    attempts: u8,
    completed: bool,
    closed: bool,
}

impl PairingGate {
    fn new(policy: Pairing) -> Self {
        Self {
            policy,
            opened: tokio::time::Instant::now(),
            attempts: 0,
            completed: false,
            closed: false,
        }
    }

    fn for_peer(&mut self, already_known: bool) -> Pairing {
        if already_known {
            return self.policy;
        }
        if !self.is_open() {
            return None;
        }
        self.attempts += 1;
        self.policy
    }

    fn paired(&mut self, trust: Trust) {
        if trust == Trust::NewlyPaired {
            self.completed = true;
        }
    }

    fn is_open(&self) -> bool {
        self.policy.is_some()
            && !self.closed
            && !self.completed
            && self.opened.elapsed() < PAIRING_WINDOW
            && self.attempts < MAX_PAIRING_ATTEMPTS
    }

    fn deadline(&self) -> Option<tokio::time::Instant> {
        self.is_open().then_some(self.opened + PAIRING_WINDOW)
    }

    fn close(&mut self) {
        self.closed = true;
    }
}

/// Wait for peers to connect until the future is cancelled, running a link
/// with each one that does.
pub async fn listen<P, O>(
    config: SessionConfig<'_>,
    bind: &str,
    port: u16,
    prompt: &mut P,
    observer: &mut O,
) -> Result<()>
where
    P: PairingPrompt + Clone + Send,
    O: ServiceObserver + Send,
{
    let listener = TcpListener::bind((bind, port))
        .await
        .with_context(|| format!("listening on {bind}:{port}"))?;
    observer.waiting(config.name, config.identity.public_key(), port, config.pairing);

    let hub = Hub::new(&config);
    let pairing = std::sync::Mutex::new(PairingGate::new(config.pairing));
    let gate = || pairing.lock().unwrap_or_else(|e| e.into_inner());
    let observer = std::sync::Mutex::new(observer);
    let busy = std::sync::Mutex::new(std::collections::HashSet::<PublicKey>::new());
    let mut discoverable = config.discoverable.clone();
    let mut running: Running<'_, ()> = Running::default();
    let mut advertiser = advertise(&config, port, &gate(), *discoverable.borrow_and_update());
    loop {
        let deadline = gate().deadline();
        tokio::select! {
            result = listener.accept() => {
                let (stream, address) = result?;
                stream.set_nodelay(true)?;
                let (config, hub, observer, pairing, busy) = (&config, &hub, &observer, &pairing, &busy);
                let mut prompt = prompt.clone();
                running.push(async move {
                    let mut shared = Shared(observer);
                    let was_open = pairing.lock().unwrap_or_else(|e| e.into_inner()).is_open();
                    match answer(stream, config, hub, pairing, &mut prompt, &mut shared, busy).await {
                        Ok(peer) => shared.disconnected(&peer),
                        Err(error) => shared.connection_failed(&address.to_string(), &error),
                    }
                    if was_open && !pairing.lock().unwrap_or_else(|e| e.into_inner()).is_open() {
                        shared.pairing_closed();
                    }
                });
            }
            () = running.next() => {}
            () = async { match deadline { Some(deadline) => tokio::time::sleep_until(deadline).await, None => std::future::pending().await } } => {
                gate().close();
                Shared(&observer).pairing_closed();
            }
            Ok(()) = discoverable.changed() => {}
        }
        // the advertisement says whether pairing is open
        let allowed = *discoverable.borrow_and_update();
        if advertiser.is_some() != allowed || advertiser.as_ref().is_some_and(|a| a.pairing != gate().is_open()) {
            drop(advertiser.take());
            advertiser = advertise(&config, port, &gate(), allowed);
        }
    }
}

/// Connect to the peer at `address` and keep the session going: once a session
/// has run, reconnect and run the handshake again whenever it drops. The
/// first connection is not retried, so a wrong address or a peer not set up
/// yet is reported. Stops on anything a retry cannot fix: trust ended, a
/// different system answering, or a local or setup error. Only the first
/// connection may pair; reconnecting never does.
pub async fn connect<P, O>(
    config: SessionConfig<'_>,
    address: &str,
    peer: Option<PublicKey>,
    prompt: &mut P,
    observer: &mut O,
) -> Result<()>
where
    P: PairingPrompt + Send,
    O: ServiceObserver + Send,
{
    let hub = Hub::new(&config);
    // a peer chosen from those found on the network is already paired: check its key from the start
    let mut expected = peer;
    let mut started = false;
    // what to call the peer while reconnecting: where it was last reached
    let mut last_reached = connect_address(address);
    let mut waits = reconnect::waits();
    loop {
        let attempt = SessionConfig {
            pairing: pairing_for_attempt(config.pairing, expected.is_some()),
            ..config
        };
        let result = match locate(expected, address).await {
            Ok(address) => {
                observer.connecting(&address, peer_name(config.peers, expected).as_deref());
                last_reached.clone_from(&address);
                let (lasted, result) = connect_once(attempt, &hub, &address, &mut expected, prompt, observer).await;
                started |= lasted.is_some();
                if lasted.is_some_and(|lasted| lasted >= reconnect::STABLE) {
                    waits = reconnect::waits();
                }
                result
            }
            Err(error) => Err(error),
        };
        match result {
            // the peer ended the session; wait for it to come back
            Ok(()) => {}
            // Until a session has worked, a failure is more likely a wrong
            // address or a peer not set up yet than a network blip: say so.
            Err(error) if !started => return Err(error),
            Err(error) if reconnect::retryable(&error) => {
                tracing::info!(error = format!("{error:#}"), "connection lost; reconnecting");
            }
            Err(error) => return Err(error),
        }
        let wait = waits.next().unwrap_or(Duration::from_secs(10));
        observer.reconnecting(&last_reached, peer_name(config.peers, expected).as_deref(), wait);
        tokio::time::sleep(wait).await;
    }
}

/// The side chosen for this start, for the first session only.
fn take_side_choice(config: &SessionConfig<'_>) -> Option<Side> {
    config.choose_side.swap(false, Ordering::AcqRel).then_some(config.side)
}

/// The name of the paired peer with `key`, if there is one.
fn peer_name(peers: &PeerStore, key: Option<PublicKey>) -> Option<String> {
    let key = key?;
    peers.trusted(&key, trust::now()).ok().flatten().map(|peer| peer.name)
}

/// This system's Bonjour advertisement while waiting, if allowed. A failure to
/// advertise is logged, not fatal: connecting by name or address still works.
fn advertise(config: &SessionConfig<'_>, port: u16, pairing: &PairingGate, allowed: bool) -> Option<Advertiser> {
    if !allowed {
        return None;
    }
    Advertiser::start(&config.identity.public_key(), port, pairing.is_open())
        .inspect_err(|error| tracing::warn!(error = format!("{error:#}"), "could not advertise with Bonjour"))
        .ok()
}

/// How long each attempt looks for a peer on the network before
/// falling back to the saved address.
const FIND_TIMEOUT: Duration = Duration::from_secs(3);

/// Where to reach the peer this attempt: found with Bonjour when its key is
/// known, otherwise the saved name or address.
async fn locate(peer: Option<PublicKey>, saved: &str) -> Result<String> {
    if let Some(key) = peer
        && let Some(found) = discovery::find(key, FIND_TIMEOUT).await
    {
        return Ok(found.to_string());
    }
    if saved.trim().is_empty() {
        return Err(Unreachable {
            address: "the peer".to_owned(),
            source: std::io::ErrorKind::NotFound.into(),
        }
        .into());
    }
    Ok(connect_address(saved))
}

/// Pairing is offered only until a session has started; reconnecting to that
/// peer never pairs, so a stranger at the same address cannot slip in.
fn pairing_for_attempt(pairing: Pairing, reconnecting: bool) -> Pairing {
    if reconnecting { None } else { pairing }
}

/// One connection: reach the peer, run the Noise handshake, settle trust, and
/// run the session until it ends. Returns how long the session ran, if it
/// started, with how it ended.
async fn connect_once<P, O>(
    config: SessionConfig<'_>,
    hub: &Hub,
    address: &str,
    expected: &mut Option<PublicKey>,
    prompt: &mut P,
    observer: &mut O,
) -> (Option<Duration>, Result<()>)
where
    P: PairingPrompt + Send,
    O: ServiceObserver + Send,
{
    let mut lasted = None;
    let result = async {
        let unreachable = |source| Unreachable {
            address: address.to_owned(),
            source,
        };
        let stream = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(address))
            .await
            .map_err(|_| unreachable(std::io::ErrorKind::TimedOut.into()))?
            .map_err(unreachable)?;
        stream.set_nodelay(true)?;
        let mut channel = tokio::time::timeout(HANDSHAKE_TIMEOUT, Channel::initiate(stream, config.identity))
            .await
            .context("the Noise handshake timed out")??;
        if expected.is_some_and(|key| key != channel.remote_key()) {
            return Err(KeyChanged {
                address: address.to_owned(),
            }
            .into());
        }
        let (peer, trust_status) = tokio::time::timeout(
            TRUST_TIMEOUT,
            settle_trust(
                &mut channel,
                config.peers,
                config.name,
                config.pairing,
                prompt,
                observer,
            ),
        )
        .await
        .context("pairing or trust negotiation timed out")??;
        if trust_status == Trust::NewlyPaired {
            config
                .peers
                .agree_side(&channel.remote_key(), config.side, trust::now())?;
        }
        *expected = Some(channel.remote_key());
        let started = tokio::time::Instant::now();
        let session = run_session(channel, &config, hub, &peer, take_side_choice(&config), observer).await;
        lasted = Some(started.elapsed());
        observer.disconnected(&peer);
        session
    }
    .await;
    (lasted, result)
}

async fn answer<P, O>(
    stream: TcpStream,
    config: &SessionConfig<'_>,
    hub: &Hub,
    pairing: &std::sync::Mutex<PairingGate>,
    prompt: &mut P,
    observer: &mut O,
    busy: &std::sync::Mutex<std::collections::HashSet<PublicKey>>,
) -> Result<String>
where
    P: PairingPrompt + Send,
    O: ServiceObserver + Send,
{
    let gate = || pairing.lock().unwrap_or_else(|e| e.into_inner());
    let mut channel = tokio::time::timeout(HANDSHAKE_TIMEOUT, Channel::respond(stream, config.identity))
        .await
        .context("the Noise handshake timed out")??;
    let key = channel.remote_key();
    let known = config.peers.trusted(&key, trust::now())?.is_some();
    let policy = gate().for_peer(known);
    let (peer, trust_status) = tokio::time::timeout(
        TRUST_TIMEOUT,
        settle_trust(&mut channel, config.peers, config.name, policy, prompt, observer),
    )
    .await
    .context("pairing or trust negotiation timed out")??;
    if trust_status == Trust::NewlyPaired {
        config.peers.agree_side(&key, config.side, trust::now())?;
    }
    gate().paired(trust_status);
    busy.lock().unwrap_or_else(|e| e.into_inner()).insert(key);
    let result = run_session(channel, config, hub, &peer, take_side_choice(config), observer).await;
    busy.lock().unwrap_or_else(|e| e.into_inner()).remove(&key);
    result?;
    Ok(peer)
}

async fn settle_trust<S, P, O>(
    channel: &mut Channel<S>,
    peers: &PeerStore,
    name: &str,
    pairing: Pairing,
    prompt: &mut P,
    observer: &mut O,
) -> Result<(String, Trust)>
where
    S: AsyncRead + AsyncWrite + Unpin,
    P: PairingPrompt + Send,
    O: ServiceObserver + Send,
{
    let policy = pairing.unwrap_or_default();
    let (peer, trust) = establish_trust(channel, peers, name, pairing.is_some(), policy, prompt).await?;
    if trust == Trust::NewlyPaired {
        observer.paired(&peer, channel.remote_key(), policy);
    }
    Ok((peer, trust))
}

async fn run_session<O>(
    mut channel: Channel<TcpStream>,
    config: &SessionConfig<'_>,
    hub: &Hub,
    peer: &str,
    chosen_side: Option<Side>,
    observer: &mut O,
) -> Result<()>
where
    O: ServiceObserver + Send,
{
    let peers = config.peers;
    let initiator = channel.role() == crate::session::Role::Initiator;
    let key = channel.remote_key();
    if let Some(side) = chosen_side {
        peers.set_side(&key, side)?;
    }
    let local = peers
        .trusted(&key, trust::now())?
        .map_or((Side::Right, 0), |peer| (peer.side, peer.side_chosen));
    channel
        .send(&crate::protocol::Message::Layout {
            side: local.0,
            chosen: local.1,
        })
        .await?;
    let remote = match tokio::time::timeout(Duration::from_secs(5), channel.recv())
        .await
        .context("the peer did not send its screen arrangement")??
    {
        crate::protocol::Message::Layout { side, chosen } => (side, chosen),
        _ => anyhow::bail!("the peer runs a different version of Daisy; update Daisy on both systems"),
    };
    let (side, chosen) = crate::control::agreed_side(initiator, local, remote);
    peers.agree_side(&key, side, chosen)?;
    let mut visit = peers.visit(key)?;
    let (done, mut ended) = tokio::sync::oneshot::channel();
    let (agreed_tx, mut agreed_rx) = mpsc::unbounded_channel();
    let mut link = hub.link();
    let _member = hub.join(share::Joining {
        channel,
        agreed: (side, chosen),
        initiator,
        agreed_tx,
        done,
    })?;
    observer.connected(peer, key, side);
    let watch = peers.watch(key, peer);
    tokio::pin!(watch);
    let mut trust_ended = None;
    let result = loop {
        tokio::select! {
            result = &mut ended => {
                let result = result.unwrap_or_else(|_| Err(anyhow::anyhow!("Daisy's input sharing stopped")));
                break match trust_ended.take() { Some(error) => Err(error), None => result };
            }
            error = &mut watch, if trust_ended.is_none() => {
                trust_ended = Some(error);
                hub.drop_link(key);
            }
            Ok(()) = link.changed() => {
                let current = *link.borrow_and_update();
                observer.link(peer, current);
            }
            Some((side, chosen)) = agreed_rx.recv() => {
                if let Err(error) = peers.agree_side(&key, side, chosen) {
                    tracing::warn!(error = ?error, "the new screen arrangement could not be saved");
                }
                observer.connected(peer, key, side);
            }
        }
    };
    if let Err(error) = &result
        && share::connection_lost(error)
    {
        visit.dropped();
    }
    result
}

/// The most systems a group holds, this one included.
pub const MAX_GROUP: usize = 8;

/// The one input core every link on this system shares: one event tap, one
/// injector and one owner of control. It starts with the first link and
/// stops after the last.
pub struct Hub {
    me: PublicKey,
    clipboard: watch::Receiver<bool>,
    choices: watch::Receiver<Option<Side>>,
    running: std::sync::Arc<std::sync::Mutex<Option<mpsc::UnboundedSender<share::Membership<TcpStream>>>>>,
    link: std::sync::Arc<watch::Sender<crate::control::Link>>,
    members: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

/// Counts a link as a member of the group while it lives.
pub struct Member(std::sync::Arc<std::sync::atomic::AtomicUsize>);

impl Drop for Member {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

impl Hub {
    pub fn new(config: &SessionConfig<'_>) -> Self {
        Self {
            me: config.identity.public_key(),
            clipboard: config.clipboard.clone(),
            choices: session_choices(config.arrangement),
            running: std::sync::Arc::default(),
            link: std::sync::Arc::new(watch::Sender::new(crate::control::Link::default())),
            members: std::sync::Arc::default(),
        }
    }

    /// Peers this system has a running link with.
    pub fn members(&self) -> usize {
        self.members.load(Ordering::Acquire)
    }

    /// Who has control and the round trip, as the group reports them.
    fn link(&self) -> watch::Receiver<crate::control::Link> {
        self.link.subscribe()
    }

    /// Whether one more system fits in the group.
    pub fn has_room(&self) -> bool {
        self.members() + 1 < MAX_GROUP
    }

    /// Adds a link to the group, starting the input core if none is running.
    fn join(&self, joining: share::Joining<TcpStream>) -> Result<Member> {
        if !self.has_room() {
            anyhow::bail!("this group already has {MAX_GROUP} systems");
        }
        self.members.fetch_add(1, Ordering::AcqRel);
        let member = Member(self.members.clone());
        let mut running = self.running.lock().unwrap_or_else(|e| e.into_inner());
        let joining = match running.as_ref() {
            Some(core) => match core.send(share::Membership::Join(joining)) {
                Ok(()) => return Ok(member),
                Err(mpsc::error::SendError(share::Membership::Join(joining))) => joining,
                Err(_) => unreachable!("only a join was sent"),
            },
            None => joining,
        };
        let (members, membership) = mpsc::unbounded_channel();
        let _ = members.send(share::Membership::Join(joining));
        *running = Some(members);
        tokio::spawn(run_core(
            self.me,
            self.clipboard.clone(),
            self.choices.clone(),
            membership,
            self.running.clone(),
            self.link.clone(),
        ));
        Ok(member)
    }

    /// Ends the link with `key`, as when its trust runs out.
    fn drop_link(&self, key: PublicKey) {
        if let Some(core) = self.running.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            let _ = core.send(share::Membership::Drop(key));
        }
    }
}

/// Runs the input core while it has links. A link that arrives as the
/// last one ends starts it again rather than being lost.
async fn run_core(
    me: PublicKey,
    clipboard: watch::Receiver<bool>,
    choices: watch::Receiver<Option<Side>>,
    mut membership: mpsc::UnboundedReceiver<share::Membership<TcpStream>>,
    running: std::sync::Arc<std::sync::Mutex<Option<mpsc::UnboundedSender<share::Membership<TcpStream>>>>>,
    link: std::sync::Arc<watch::Sender<crate::control::Link>>,
) {
    let mut waiting = std::collections::VecDeque::new();
    loop {
        waiting.extend(std::iter::from_fn(|| membership.try_recv().ok()));
        if let Err(error) = run_core_once(me, &clipboard, &choices, &mut waiting, &mut membership, &link).await {
            tracing::warn!(error = format!("{error:#}"), "input sharing stopped");
            let reason = format!("{error:#}");
            for change in waiting
                .drain(..)
                .chain(std::iter::from_fn(|| membership.try_recv().ok()))
            {
                if let share::Membership::Join(joining) = change {
                    let _ = joining.done.send(Err(anyhow::anyhow!("{reason}")));
                }
            }
        }
        let mut slot = running.lock().unwrap_or_else(|e| e.into_inner());
        waiting.extend(std::iter::from_fn(|| membership.try_recv().ok()));
        if waiting.is_empty() {
            *slot = None;
            return;
        }
    }
}

async fn run_core_once(
    me: PublicKey,
    clipboard: &watch::Receiver<bool>,
    choices: &watch::Receiver<Option<Side>>,
    waiting: &mut std::collections::VecDeque<share::Membership<TcpStream>>,
    membership: &mut mpsc::UnboundedReceiver<share::Membership<TcpStream>>,
    link: &watch::Sender<crate::control::Link>,
) -> Result<()> {
    let displays = macos::displays()?;
    let control = std::sync::Arc::new(crate::control::SharedControl::new(me, me));
    let mut reports = control.watch_link();
    let mut injector = Injector::new();
    let mut sharing = Sharing::new(Pasteboard, clipboard.clone());
    let (messages, input) = mpsc::channel(INPUT_QUEUE_CAPACITY);
    let alone = crate::layout::Group::alone(me, displays.clone());
    let (mut capture, overflowed) = Capture::start(alone, me, messages, control.clone())?;
    let until = async {
        loop {
            if overflowed.load(Ordering::Acquire) {
                break anyhow::anyhow!("local input queue overloaded; control was reclaimed");
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    };
    let group = share::Group {
        displays,
        control,
        choices: choices.clone(),
    };
    let run = share::run(
        group,
        std::mem::take(waiting),
        membership,
        input,
        &mut capture,
        &mut injector,
        &mut sharing,
        until,
    );
    tokio::pin!(run);
    loop {
        tokio::select! {
            result = &mut run => return result,
            Ok(()) = reports.changed() => { link.send_replace(*reports.borrow_and_update()); }
        }
    }
}

/// Sides chosen on this system during a session. Without a way to choose,
/// as from the command line, it never changes, and the peer's choices still
/// apply.
fn session_choices(arrangement: Option<&watch::Receiver<Option<Side>>>) -> watch::Receiver<Option<Side>> {
    let mut choices = arrangement.cloned().unwrap_or_else(|| watch::channel(None).1);
    // only choices made during this session count
    choices.mark_unchanged();
    choices
}

/// Add the default Daisy port when the address has none.
pub fn connect_address(address: &str) -> String {
    if let Ok(ip) = address.parse::<IpAddr>() {
        return match ip {
            IpAddr::V4(_) => format!("{address}:{DEFAULT_PORT}"),
            IpAddr::V6(_) => format!("[{address}]:{DEFAULT_PORT}"),
        };
    }
    if address.starts_with('[') && address.ends_with(']') {
        return format!("{address}:{DEFAULT_PORT}");
    }
    if address.contains(':') {
        address.to_owned()
    } else {
        format!("{address}:{DEFAULT_PORT}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn addresses_get_default_port_only_when_needed() {
        assert_eq!(connect_address("studio.local"), "studio.local:24850");
        assert_eq!(connect_address("studio.local:1234"), "studio.local:1234");
        assert_eq!(connect_address("127.0.0.1"), "127.0.0.1:24850");
        assert_eq!(connect_address("::1"), "[::1]:24850");
        assert_eq!(connect_address("[::1]"), "[::1]:24850");
        assert_eq!(connect_address("[::1]:1234"), "[::1]:1234");
    }

    #[tokio::test(start_paused = true)]
    async fn pairing_gate_limits_unknown_peers_and_closes_after_success() {
        let mut gate = PairingGate::new(Some(Policy::IDLE));
        assert!(gate.is_open());
        for _ in 0..MAX_PAIRING_ATTEMPTS {
            assert_eq!(gate.for_peer(false), Some(Policy::IDLE));
        }
        assert!(!gate.is_open());
        assert_eq!(gate.for_peer(false), None);
        assert_eq!(gate.for_peer(true), Some(Policy::IDLE));

        let mut gate = PairingGate::new(Some(Policy::IDLE));
        gate.paired(Trust::NewlyPaired);
        assert!(!gate.is_open());
        assert_eq!(gate.for_peer(false), None);

        let mut gate = PairingGate::new(Some(Policy::IDLE));
        tokio::time::advance(PAIRING_WINDOW).await;
        assert!(!gate.is_open());
        assert_eq!(gate.for_peer(false), None);

        let mut gate = PairingGate::new(Some(Policy::IDLE));
        gate.close();
        assert!(!gate.is_open());
    }

    #[derive(Default)]
    struct Recorded {
        waits: Vec<Duration>,
        reconnecting_to: Vec<String>,
    }

    impl ServiceObserver for Recorded {
        fn reconnecting(&mut self, address: &str, _peer: Option<&str>, wait: Duration) {
            self.waits.push(wait);
            self.reconnecting_to.push(address.to_owned());
        }
    }

    #[derive(Clone)]
    struct NoCodes;

    impl PairingPrompt for NoCodes {
        fn show_code(&mut self, _code: &crate::pairing::PairingCode, _peer: &str) {}
        async fn ask_code(&mut self, _peer: &str) -> Result<crate::pairing::PairingCode> {
            anyhow::bail!("no code")
        }
    }

    struct System {
        _home: tempfile::TempDir,
        identity: Identity,
        peers: PeerStore,
    }

    fn system() -> System {
        let home = tempfile::tempdir().unwrap();
        let peers = PeerStore::open(home.path()).unwrap();
        System {
            identity: Identity::generate().unwrap(),
            peers,
            _home: home,
        }
    }

    fn config<'a>(system: &'a System, clipboard: &'a watch::Receiver<bool>) -> SessionConfig<'a> {
        let discoverable: &'a watch::Receiver<bool> = Box::leak(Box::new(watch::channel(false).1));
        SessionConfig {
            identity: &system.identity,
            peers: &system.peers,
            name: "Laptop",
            pairing: None,
            side: Side::Right,
            choose_side: Box::leak(Box::new(AtomicBool::new(false))),
            clipboard,
            discoverable,
            arrangement: None,
        }
    }

    #[test]
    fn a_chosen_side_applies_to_the_first_session_only() {
        let system = system();
        let clipboard = watch::channel(true).1;
        let chosen = AtomicBool::new(true);
        let config = SessionConfig {
            side: Side::Above,
            choose_side: &chosen,
            ..config(&system, &clipboard)
        };
        assert_eq!(take_side_choice(&config), Some(Side::Above));
        assert_eq!(take_side_choice(&config), None);
    }

    #[test]
    fn only_the_first_connection_may_pair() {
        assert_eq!(pairing_for_attempt(Some(Policy::IDLE), false), Some(Policy::IDLE));
        assert_eq!(pairing_for_attempt(Some(Policy::IDLE), true), None);
        assert_eq!(pairing_for_attempt(None, false), None);
    }

    #[tokio::test]
    async fn a_peer_that_was_never_reached_is_reported_not_retried() {
        let mut prompt = NoCodes;
        // a port that was just free: nothing is listening on it
        let port = TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let here = system();
        let clipboard = watch::channel(true).1;
        let mut observer = Recorded::default();
        let address = format!("127.0.0.1:{port}");
        let attempt = connect(config(&here, &clipboard), &address, None, &mut prompt, &mut observer);
        let error = tokio::time::timeout(Duration::from_secs(4), attempt)
            .await
            .expect("the first failure should end connect")
            .unwrap_err();
        assert!(error.downcast_ref::<Unreachable>().is_some(), "{error:#}");
        assert!(observer.waits.is_empty(), "{:?}", observer.waits);
    }

    #[tokio::test]
    async fn a_trusted_connection_dropped_during_layout_is_reconnected() {
        let mut prompt = NoCodes;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let (here, there) = (system(), system());
        let now = trust::now();
        here.peers
            .pin(there.identity.public_key(), "Studio", Policy::IDLE, now)
            .unwrap();
        there
            .peers
            .pin(here.identity.public_key(), "Laptop", Policy::IDLE, now)
            .unwrap();
        let clipboard = watch::channel(true).1;
        let mut observer = Recorded::default();
        // A trusted peer closes during layout agreement, then accepts again.
        let server = async {
            let mut sessions = 0;
            while sessions < 2 {
                let (stream, _) = listener.accept().await.unwrap();
                let mut channel = Channel::respond(stream, &there.identity).await.unwrap();
                establish_trust(&mut channel, &there.peers, "Studio", false, Policy::IDLE, &mut NoCodes)
                    .await
                    .unwrap();
                sessions += 1;
                drop(channel);
            }
            sessions
        };
        let client = tokio::time::timeout(
            Duration::from_secs(8),
            connect(config(&here, &clipboard), &address, None, &mut prompt, &mut observer),
        );
        let (sessions, _) = tokio::time::timeout(Duration::from_secs(10), async { tokio::join!(server, client) })
            .await
            .unwrap();
        assert_eq!(sessions, 2);
        assert!(!observer.waits.is_empty());
        assert!(
            observer.reconnecting_to.iter().all(|to| *to == address),
            "{:?}",
            observer.reconnecting_to
        );
    }

    // Uses the real network stack: the waiting system advertises with Bonjour
    // and the other finds it from its key alone, with no address.
    #[tokio::test]
    #[ignore = "uses the local network; run by hand with --ignored"]
    async fn a_paired_peer_is_found_and_connected_without_an_address() {
        let mut prompt = NoCodes;
        let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (here, there) = (system(), system());
        let now = trust::now();
        here.peers
            .pin(there.identity.public_key(), "Studio", Policy::IDLE, now)
            .unwrap();
        there
            .peers
            .pin(here.identity.public_key(), "Laptop", Policy::IDLE, now)
            .unwrap();
        let _advertiser = Advertiser::start(&there.identity.public_key(), port, false).unwrap();
        let server = async {
            let (stream, _) = listener.accept().await.unwrap();
            let mut channel = Channel::respond(stream, &there.identity).await.unwrap();
            establish_trust(&mut channel, &there.peers, "Studio", false, Policy::IDLE, &mut NoCodes)
                .await
                .unwrap()
                .0
        };
        let clipboard = watch::channel(true).1;
        let mut observer = Recorded::default();
        let client = tokio::time::timeout(
            Duration::from_secs(10),
            connect(
                config(&here, &clipboard),
                "",
                Some(there.identity.public_key()),
                &mut prompt,
                &mut observer,
            ),
        );
        let (peer, _) = tokio::time::timeout(Duration::from_secs(12), async { tokio::join!(server, client) })
            .await
            .expect("the peer was never reached");
        assert_eq!(peer, "Laptop");
    }

    #[tokio::test]
    async fn a_peer_that_no_longer_trusts_this_one_stops_the_retries() {
        let mut prompt = NoCodes;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let there = system();
        let server = async {
            let (stream, _) = listener.accept().await.unwrap();
            let mut channel = Channel::respond(stream, &there.identity).await.unwrap();
            let _ = establish_trust(&mut channel, &there.peers, "Studio", false, Policy::IDLE, &mut NoCodes).await;
        };
        let here = system();
        let clipboard = watch::channel(true).1;
        let mut observer = Recorded::default();
        let client = tokio::time::timeout(
            Duration::from_secs(5),
            connect(config(&here, &clipboard), &address, None, &mut prompt, &mut observer),
        );
        let (_, result) = tokio::join!(server, client);
        let error = result.expect("connect gave up instead of looping").unwrap_err();
        assert!(format!("{error:#}").contains("not paired"), "{error:#}");
        assert!(observer.waits.is_empty(), "{:?}", observer.waits);
    }

    #[tokio::test]
    async fn links_run_side_by_side_and_borrow_from_their_task() {
        let shared = std::sync::Mutex::new(Vec::new());
        let mut running: Running<'_, u8> = Running::default();
        let (slow, fast) = (
            tokio::sync::oneshot::channel::<()>(),
            tokio::sync::oneshot::channel::<()>(),
        );
        let (release_slow, wait_slow) = slow;
        let (release_fast, wait_fast) = fast;
        let shared_ref = &shared;
        running.push(async move {
            let _ = wait_slow.await;
            shared_ref.lock().unwrap().push(1);
            1
        });
        running.push(async move {
            let _ = wait_fast.await;
            shared_ref.lock().unwrap().push(2);
            2
        });
        release_fast.send(()).unwrap();
        assert_eq!(running.next().await, 2);
        release_slow.send(()).unwrap();
        assert_eq!(running.next().await, 1);
        assert_eq!(*shared.lock().unwrap(), [2, 1]);
        let never = tokio::time::timeout(Duration::from_millis(20), running.next()).await;
        assert!(never.is_err(), "an empty set never finishes");
    }

    #[tokio::test]
    async fn a_full_group_refuses_another_system() {
        let here = system();
        let clipboard = watch::channel(true).1;
        let hub = Hub::new(&config(&here, &clipboard));
        let members: Vec<Member> = (0..MAX_GROUP - 1)
            .map(|_| {
                hub.members.fetch_add(1, Ordering::AcqRel);
                Member(hub.members.clone())
            })
            .collect();
        assert_eq!(hub.members(), MAX_GROUP - 1);
        assert!(!hub.has_room());
        drop(members);
        assert_eq!(hub.members(), 0);
        assert!(hub.has_room());
    }

    #[tokio::test]
    async fn a_different_peer_at_the_address_is_refused() {
        let mut prompt = NoCodes;
        let mut observer = Recorded::default();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let impostor = system();
        let server = async {
            let (stream, _) = listener.accept().await.unwrap();
            let _ = Channel::respond(stream, &impostor.identity).await;
        };
        let here = system();
        let clipboard = watch::channel(true).1;
        let mut expected = Some(Identity::generate().unwrap().public_key());
        let hub = Hub::new(&config(&here, &clipboard));
        let client = connect_once(
            config(&here, &clipboard),
            &hub,
            &address,
            &mut expected,
            &mut prompt,
            &mut observer,
        );
        let (_, (lasted, result)) = tokio::join!(server, client);
        let error = result.unwrap_err();
        assert!(error.downcast_ref::<KeyChanged>().is_some(), "{error:#}");
        assert!(!reconnect::retryable(&error));
        assert_eq!(lasted, None);
    }
}
