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
/// How long after a failed or cancelled pairing this system waits before
/// opening another.
const PAIRING_QUIET: Duration = Duration::from_secs(15);
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
    let mut listening = config.listening.cloned();
    let has_peers = || config.peers.list(trust::now()).map(|peers| !peers.is_empty());
    let initial = listening.as_mut().map_or(Listening::Closed, |l| *l.borrow_and_update());
    let pairing = std::sync::Mutex::new(PairingGate::listening(config.pairing, initial, has_peers()?));
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
                        if let Err(error) = result {
                            shared.connection_failed(&address.to_string(), &error);
                        }
                        None
                    });
                }
                finished = running.next() => {
                    if let Some(opened) = finished {
                        if opened.pairing && !opened.paired { gate().exchanged(false); }
                        if let Err(error) = opened.result { Shared(&observer).connection_failed(&opened.address, &error); }
                        if let Some(key) = opened.key { busy.lock().unwrap_or_else(|e| e.into_inner()).remove(&key); }
                    }
                    if hub.members() == 0 { break; }
                }
                changed = browser.changed() => {
                    if !changed { anyhow::bail!("Bonjour browsing stopped"); }
                }
                Ok(()) = discoverable.changed() => break,
                Some(now) = listening_changed(&mut listening) => {
                    let was_open = gate().is_open();
                    *gate() = PairingGate::listening(config.pairing, now, has_peers()?);
                    match now {
                        Listening::For(_) => Shared(&observer).pairing_opened(),
                        _ if was_open && !gate().is_open() => Shared(&observer).pairing_closed(),
                        _ => {}
                    }
                    // advertise again, with the new offer
                    break;
                }
                _ = scan.tick() => {
                    let keys: Vec<_> = config.peers.list(trust::now())?.iter().map(|p| p.key).collect();
                    gate().tick();
                    if gate().ended() {
                        gate().close();
                        Shared(&observer).pairing_closed();
                        break;
                    }
                    // membership changes as systems pair and are forgotten;
                    // a changed offer is advertised again below
                    gate().member = !keys.is_empty();
                    // a system whose last peer was forgotten is new again
                    if keys.is_empty() && gate().closed && config.pairing.is_some() {
                        *gate() = PairingGate::listening(config.pairing, Listening::Closed, false);
                        break;
                    }
                    if advertiser.as_ref().is_some_and(|a| a.offer != gate().offer()) { break; }
                    if !hub.has_room() { continue; }
                    let own = config.identity.public_key();
                    let mut found = browser.current(&keys);
                    found.sort_by_key(|heard| heard.election);
                    let now = tokio::time::Instant::now();
                    first_seen.retain(|election, _| found.iter().any(|heard| heard.election == *election));
                    for heard in &found { first_seen.entry(heard.election).or_insert(now); }
                    let election = advertiser.as_ref().map(|a| a.election);
                    let offer = if gate().may_open() { gate().offer() } else { discovery::Offer::Closed };
                    for heard in found {
                        let seen_for = now - first_seen[&heard.election];
                        if !discovery::opens_connection(own, election, &heard, offer, seen_for) { continue; }
                        let expected = match heard.seen { discovery::Seen::Paired(key) => Some(key), discovery::Seen::Pairing { .. } => None };
                        if let Some(key) = expected && !busy.lock().unwrap_or_else(|e| e.into_inner()).insert(key) { continue; }
                        let policy = gate().for_peer(expected.is_some());
                        if expected.is_none() && policy.is_none() { continue; }
                        // wait out the grace again, so two systems that both
                        // connected do not keep colliding
                        first_seen.remove(&heard.election);
                        let attempt = SessionConfig { pairing: policy, ..config };
                        let address = heard.address.to_string();
                        Shared(&observer).connecting(&address, peer_name(config.peers, expected).as_deref());
                        let (hub, observer) = (&hub, &observer);
                        let mut prompt = prompt.clone();
                        let gate_lock = &pairing;
                        let began_pairing = expected.is_none();
                        running.push(async move {
                            let mut expected = expected;
                            let mut watched = GateWatch { inner: Shared(observer), gate: gate_lock, paired: false };
                            let (_, result) = connect_once(attempt, hub, &address, &mut expected, &mut prompt, &mut watched).await;
                            Some(Opened { address, key: expected, pairing: began_pairing, paired: watched.paired, result })
                        });
                    }
                }
            }
        }
    }
}

/// Each new choice of when to accept a new system.
async fn listening_changed(listening: &mut Option<watch::Receiver<Listening>>) -> Option<Listening> {
    let Some(listening) = listening else {
        return std::future::pending().await;
    };
    if listening.changed().await.is_err() {
        return std::future::pending().await;
    }
    Some(*listening.borrow_and_update())
}

/// How a link this system opened ended.
struct Opened {
    address: String,
    /// The peer, once known.
    key: Option<PublicKey>,
    /// Whether it began as a pairing.
    pairing: bool,
    /// Whether that pairing succeeded.
    paired: bool,
    result: Result<()>,
}

/// Passes everything on, and ends this system's pairing exchange as soon as
/// a new system is paired rather than when its link ends.
struct GateWatch<'a, O> {
    inner: O,
    gate: &'a std::sync::Mutex<PairingGate>,
    paired: bool,
}

impl<O: ServiceObserver> ServiceObserver for GateWatch<'_, O> {
    fn waiting(&mut self, name: &str, key: PublicKey, port: u16, pairing: Pairing) {
        self.inner.waiting(name, key, port, pairing);
    }
    fn connecting(&mut self, address: &str, peer: Option<&str>) {
        self.inner.connecting(address, peer);
    }
    fn reconnecting(&mut self, address: &str, peer: Option<&str>, wait: Duration) {
        self.inner.reconnecting(address, peer, wait);
    }
    fn paired(&mut self, peer: &str, key: PublicKey, policy: Policy) {
        self.paired = true;
        self.gate.lock().unwrap_or_else(|e| e.into_inner()).exchanged(true);
        self.inner.paired(peer, key, policy);
    }
    fn connected(&mut self, peer: &str, key: PublicKey, side: Side) {
        self.inner.connected(peer, key, side);
    }
    fn link(&mut self, peer: &str, key: PublicKey, link: crate::control::Link) {
        self.inner.link(peer, key, link);
    }
    fn arranged(&mut self, layout: &share::Layout) {
        self.inner.arranged(layout);
    }
    fn disconnected(&mut self, peer: &str, key: PublicKey) {
        self.inner.disconnected(peer, key);
    }
    fn pairing_opened(&mut self) {
        self.inner.pairing_opened();
    }
    fn pairing_closed(&mut self) {
        self.inner.pairing_closed();
    }
    fn connection_failed(&mut self, address: &str, error: &anyhow::Error) {
        self.inner.connection_failed(address, error);
    }
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
    fn link(&mut self, peer: &str, key: PublicKey, link: crate::control::Link) {
        self.lock().link(peer, key, link);
    }
    fn arranged(&mut self, layout: &share::Layout) {
        self.lock().arranged(layout);
    }
    fn disconnected(&mut self, peer: &str, key: PublicKey) {
        self.lock().disconnected(peer, key);
    }
    fn pairing_opened(&mut self) {
        self.lock().pairing_opened();
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
    /// Latency or who has control changed on the link with `peer`.
    fn link(&mut self, _peer: &str, _key: PublicKey, _link: crate::control::Link) {}
    /// Where every member's displays now sit.
    fn arranged(&mut self, _layout: &share::Layout) {}
    /// A link that `connected` reported ended.
    fn disconnected(&mut self, _peer: &str, _key: PublicKey) {}
    /// Pairing opened while the group runs.
    fn pairing_opened(&mut self) {}
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
    /// Members moved on this system while a session runs, if it can rearrange.
    pub arrangement: Option<&'a watch::Receiver<Option<share::Placing>>>,
    /// When a running group accepts a new system.
    pub listening: Option<&'a watch::Receiver<Listening>>,
    /// Signs this system's introductions and revocations.
    pub signer: &'a crate::introduce::Signer,
}

/// When a running group accepts a system it has not paired with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Listening {
    /// Only while this system has no peers yet.
    Closed,
    /// For this long, or until one system pairs.
    For(Duration),
    /// Whenever this system is sharing.
    Always,
}

struct PairingGate {
    policy: Pairing,
    opened: tokio::time::Instant,
    /// When it closes; `None` stays open while sharing.
    until: Option<tokio::time::Instant>,
    /// Stays open after a system pairs.
    keep_open: bool,
    /// Whether this system already has peers when it opened.
    member: bool,
    /// When it opened, in Unix seconds, as the advertisement says.
    since: u64,
    attempts: u8,
    completed: bool,
    closed: bool,
    /// A pairing exchange is under way. Typing a code outlasts the opener
    /// grace, so the other system would otherwise open a second exchange
    /// and ask for a second code.
    exchanging: bool,
    /// No new exchange is opened before this, after one failed or was
    /// cancelled, so a dismissed prompt does not come straight back.
    quiet_until: Option<tokio::time::Instant>,
}

impl PairingGate {
    /// Open for the usual pairing window, as `listen --pair` is.
    fn new(policy: Pairing) -> Self {
        Self::window(policy, PAIRING_WINDOW, false)
    }

    fn window(policy: Pairing, length: Duration, member: bool) -> Self {
        let opened = tokio::time::Instant::now();
        Self {
            until: Some(opened + length),
            ..Self::open(policy, false, member)
        }
    }

    fn open(policy: Pairing, keep_open: bool, member: bool) -> Self {
        Self {
            policy,
            opened: tokio::time::Instant::now(),
            until: None,
            keep_open,
            member,
            since: discovery::unix_now(),
            attempts: 0,
            completed: false,
            closed: false,
            exchanging: false,
            quiet_until: None,
        }
    }

    fn shut(policy: Pairing) -> Self {
        Self {
            closed: true,
            ..Self::open(policy, false, true)
        }
    }

    /// The gate a session starts with or switches to: `listening` for a
    /// group, and open until the first pairing for a system with no peers.
    fn listening(policy: Pairing, listening: Listening, member: bool) -> Self {
        match listening {
            Listening::Always => Self::open(policy, true, member),
            Listening::For(length) => Self::window(policy, length, member),
            Listening::Closed if !member => Self::open(policy, false, false),
            Listening::Closed => Self::shut(policy),
        }
    }

    fn for_peer(&mut self, already_known: bool) -> Pairing {
        if already_known {
            return self.policy;
        }
        if !self.is_open() || self.exchanging {
            return None;
        }
        self.attempts += 1;
        self.exchanging = true;
        self.policy
    }

    /// Whether this system may open a connection to a system open to pairing.
    fn may_open(&self) -> bool {
        self.is_open()
            && !self.exchanging
            && self
                .quiet_until
                .is_none_or(|until| tokio::time::Instant::now() >= until)
    }

    /// The exchange `for_peer` began has ended; `paired` says whether a new
    /// system was paired.
    fn exchanged(&mut self, paired: bool) {
        self.exchanging = false;
        if paired {
            self.paired(Trust::NewlyPaired);
        } else {
            self.quiet_until = Some(tokio::time::Instant::now() + PAIRING_QUIET);
        }
    }

    fn paired(&mut self, trust: Trust) {
        if trust == Trust::NewlyPaired {
            self.completed = true;
            // an always open gate stays open, now as a member's
            self.member = true;
        }
    }

    /// Starts a gate that stays open on a fresh allowance of attempts each
    /// pairing window.
    fn tick(&mut self) {
        if self.until.is_none() && self.opened.elapsed() >= PAIRING_WINDOW {
            self.opened = tokio::time::Instant::now();
            self.attempts = 0;
        }
    }

    fn is_open(&self) -> bool {
        self.policy.is_some()
            && !self.closed
            && (self.keep_open || !self.completed)
            && self.until.is_none_or(|until| tokio::time::Instant::now() < until)
            && self.attempts < MAX_PAIRING_ATTEMPTS
    }

    /// Whether it is over for good, rather than out of attempts for now.
    fn ended(&self) -> bool {
        self.policy.is_some()
            && !self.closed
            && ((self.completed && !self.keep_open)
                || self
                    .until
                    .is_some_and(|until| tokio::time::Instant::now() >= until || self.attempts >= MAX_PAIRING_ATTEMPTS))
    }

    fn offer(&self) -> discovery::Offer {
        if self.is_open() {
            discovery::Offer::Open {
                member: self.member,
                since: self.since,
            }
        } else {
            discovery::Offer::Closed
        }
    }

    fn deadline(&self) -> Option<tokio::time::Instant> {
        self.until.filter(|_| self.is_open())
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
                    if let Err(error) = answer(stream, config, hub, pairing, &mut prompt, &mut shared, busy).await {
                        shared.connection_failed(&address.to_string(), &error);
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
        if advertiser.is_some() != allowed || advertiser.as_ref().is_some_and(|a| a.offer != gate().offer()) {
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
    Advertiser::start(&config.identity.public_key(), port, pairing.offer())
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
        let channel_key = channel.remote_key();
        let started = tokio::time::Instant::now();
        let newly_paired = trust_status == Trust::NewlyPaired;
        let session = run_session(
            channel,
            &config,
            hub,
            &peer,
            take_side_choice(&config),
            newly_paired,
            observer,
        )
        .await;
        lasted = Some(started.elapsed());
        observer.disconnected(&peer, channel_key);
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
    let settled = tokio::time::timeout(
        TRUST_TIMEOUT,
        settle_trust(&mut channel, config.peers, config.name, policy, prompt, observer),
    )
    .await;
    let paired = matches!(settled, Ok(Ok((_, Trust::NewlyPaired))));
    if !known && policy.is_some() {
        gate().exchanged(paired);
    }
    let (peer, trust_status) = settled.context("pairing or trust negotiation timed out")??;
    if trust_status == Trust::NewlyPaired {
        config.peers.agree_side(&key, config.side, trust::now())?;
    }
    gate().paired(trust_status);
    busy.lock().unwrap_or_else(|e| e.into_inner()).insert(key);
    let newly_paired = trust_status == Trust::NewlyPaired;
    let result = run_session(
        channel,
        config,
        hub,
        &peer,
        take_side_choice(config),
        newly_paired,
        observer,
    )
    .await;
    busy.lock().unwrap_or_else(|e| e.into_inner()).remove(&key);
    observer.disconnected(&peer, key);
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
    newly_paired: bool,
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
    channel
        .send(&crate::protocol::Message::SigningKey {
            key: config.signer.public(),
        })
        .await?;
    match tokio::time::timeout(Duration::from_secs(5), channel.recv())
        .await
        .context("the peer did not send its signing key")??
    {
        crate::protocol::Message::SigningKey { key: signing } => peers.set_signing(&key, signing)?,
        _ => anyhow::bail!("the peer runs a different version of Daisy; update Daisy on both systems"),
    }
    for message in catch_up(config, key)? {
        channel.send(&message).await?;
    }
    if newly_paired && let Some(newcomer) = introduction_of(config, key)? {
        hub.send(newcomer);
    }
    let mut visit = peers.visit(key)?;
    let (done, mut ended) = tokio::sync::oneshot::channel();
    let (agreed_tx, mut agreed_rx) = mpsc::unbounded_channel();
    let mut reports = hub.reports();
    let mut arranged = hub.arranged.subscribe();
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
            Ok(()) = reports.changed() => {
                let current = reports.borrow_and_update().get(&key).copied();
                if let Some(current) = current {
                    observer.link(peer, key, current);
                }
            }
            Ok(()) = arranged.changed() => {
                let layout = arranged.borrow_and_update().clone();
                observer.arranged(&layout);
            }
            Some((side, chosen)) = agreed_rx.recv() => {
                if let Err(error) = peers.agree_side(&key, side, chosen) {
                    tracing::warn!(error = ?error, "the new screen arrangement could not be saved");
                }
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

struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Acts on introductions and revocations members send: each must be signed
/// by the member that sent it, with the signing key it presented.
async fn handle_trust(hub: Hub, mut received: mpsc::UnboundedReceiver<(PublicKey, crate::protocol::Message)>) {
    while let Some((from, message)) = received.recv().await {
        if let Err(error) = act_on_trust(&hub, from, message) {
            tracing::warn!(error = format!("{error:#}"), "a member's trust message was refused");
        }
    }
}

fn act_on_trust(hub: &Hub, from: PublicKey, message: crate::protocol::Message) -> Result<()> {
    let now = trust::now();
    let signing = hub
        .peers
        .trusted(&from, now)?
        .and_then(|peer| peer.signing)
        .context("the member's signing key is not known yet")?;
    match message {
        crate::protocol::Message::Introduce { introduction } => {
            if introduction.body.introducer != from {
                anyhow::bail!("a member introduced a system on another's behalf");
            }
            let introduction = introduction.verify(&signing, now)?;
            if introduction.newcomer != hub.me && hub.peers.introduce(introduction, now)? {
                tracing::info!(name = introduction.name, "a member introduced a system");
            }
        }
        crate::protocol::Message::Revoke { revocation } => {
            // members pass revocations on, so the signer may not be the sender
            let by = revocation.body.by;
            let signing = if by == from {
                signing
            } else {
                hub.peers
                    .trusted(&by, now)?
                    .and_then(|peer| peer.signing)
                    .context("the revoking member is not trusted here")?
            };
            revocation.verify(&signing, now)?;
            if revocation.body.revoked == hub.me {
                return Ok(());
            }
            if let Some(removed) = hub.peers.revoke(revocation.clone(), now)? {
                for key in removed {
                    hub.drop_link(key);
                }
                hub.send(crate::protocol::Message::Revoke { revocation });
            }
        }
        _ => {}
    }
    Ok(())
}

/// Sends every member each revocation made here while the group runs, as
/// when someone forgets a system.
async fn send_revocations(hub: Hub) {
    let mut sent = Unsent::new(hub.peers.revocations());
    let mut every = tokio::time::interval(Duration::from_secs(2));
    loop {
        every.tick().await;
        for revocation in sent.new_ones(hub.peers.revocations()) {
            hub.send(crate::protocol::Message::Revoke { revocation });
        }
    }
}

type Revocation = crate::introduce::Signed<crate::introduce::Revocation>;

/// Revocations already sent, by what they say: the stored list is capped, so
/// its length stops growing once full.
struct Unsent(std::collections::HashSet<(PublicKey, PublicKey, trust::Timestamp)>);

impl Unsent {
    fn new(known: Vec<Revocation>) -> Self {
        Self(known.iter().map(Self::identity).collect())
    }

    fn identity(revocation: &Revocation) -> (PublicKey, PublicKey, trust::Timestamp) {
        (revocation.body.by, revocation.body.revoked, revocation.body.at)
    }

    /// The revocations in `known` not sent before.
    fn new_ones(&mut self, known: Vec<Revocation>) -> Vec<Revocation> {
        known
            .into_iter()
            .filter(|revocation| self.0.insert(Self::identity(revocation)))
            .collect()
    }
}

/// The most systems a group holds, this one included.
pub const MAX_GROUP: usize = 8;

/// The one input core every link on this system shares: one event tap, one
/// injector and one owner of control. It starts with the first link and
/// stops after the last.
#[derive(Clone)]
pub struct Hub {
    me: PublicKey,
    peers: PeerStore,
    clipboard: watch::Receiver<bool>,
    choices: watch::Receiver<Option<share::Placing>>,
    running: std::sync::Arc<std::sync::Mutex<Option<mpsc::UnboundedSender<share::Membership<TcpStream>>>>>,
    reports: std::sync::Arc<watch::Sender<share::Reports>>,
    arranged: std::sync::Arc<watch::Sender<share::Layout>>,
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
            peers: config.peers.clone(),
            clipboard: config.clipboard.clone(),
            choices: session_choices(config.arrangement),
            running: std::sync::Arc::default(),
            reports: std::sync::Arc::new(watch::Sender::new(share::Reports::new())),
            arranged: std::sync::Arc::new(watch::Sender::new(share::Layout { members: Vec::new() })),
            members: std::sync::Arc::default(),
        }
    }

    /// Peers this system has a running link with.
    pub fn members(&self) -> usize {
        self.members.load(Ordering::Acquire)
    }

    /// Each link as the group reports it.
    fn reports(&self) -> watch::Receiver<share::Reports> {
        self.reports.subscribe()
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
        tokio::spawn(run_core(self.clone(), membership));
        Ok(member)
    }

    /// Ends the link with `key`, as when its trust runs out.
    fn drop_link(&self, key: PublicKey) {
        if let Some(core) = self.running.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            let _ = core.send(share::Membership::Drop(key));
        }
    }

    /// Sends `message` to every member with a running link.
    fn send(&self, message: crate::protocol::Message) {
        if let Some(core) = self.running.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            let _ = core.send(share::Membership::Send(message));
        }
    }
}

/// Runs the input core while it has links. A link that arrives as the
/// last one ends starts it again rather than being lost.
async fn run_core(hub: Hub, mut membership: mpsc::UnboundedReceiver<share::Membership<TcpStream>>) {
    let mut waiting = std::collections::VecDeque::new();
    loop {
        waiting.extend(std::iter::from_fn(|| membership.try_recv().ok()));
        if let Err(error) = run_core_once(&hub, &mut waiting, &mut membership).await {
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
        let mut slot = hub.running.lock().unwrap_or_else(|e| e.into_inner());
        waiting.extend(std::iter::from_fn(|| membership.try_recv().ok()));
        if waiting.is_empty() {
            *slot = None;
            return;
        }
    }
}

async fn run_core_once(
    hub: &Hub,
    waiting: &mut std::collections::VecDeque<share::Membership<TcpStream>>,
    membership: &mut mpsc::UnboundedReceiver<share::Membership<TcpStream>>,
) -> Result<()> {
    let (me, peers, clipboard, choices, reports) = (hub.me, &hub.peers, &hub.clipboard, &hub.choices, &hub.reports);
    let displays = macos::displays()?;
    let control = std::sync::Arc::new(crate::control::SharedControl::new(me, me));
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
    let (shown, watched) = watch::channel(displays);
    // macOS has no display change signal a tokio task can wait on cheaply;
    // listing the displays is cheap enough to do every second
    let watcher = tokio::spawn(async move {
        let mut every = tokio::time::interval(Duration::from_secs(1));
        loop {
            every.tick().await;
            if let Ok(now) = macos::displays() {
                shown.send_if_modified(|displays| {
                    let changed = *displays != now;
                    *displays = now;
                    changed
                });
            }
        }
    });
    let _watcher = AbortOnDrop(watcher);
    let (lock, locked) = watch::channel(macos::power::screen_locked());
    let _lock_watcher = AbortOnDrop(tokio::spawn(async move {
        let mut every = tokio::time::interval(Duration::from_secs(1));
        loop {
            every.tick().await;
            lock.send_if_modified(|locked| {
                let now = macos::power::screen_locked();
                std::mem::replace(locked, now) != now
            });
        }
    }));
    // held while any link runs; displays may still sleep
    let _awake = macos::power::KeepAwake::new(c"Daisy is sharing input with a group")
        .inspect_err(|error| tracing::warn!(error = format!("{error:#}"), "could not keep this system awake"))
        .ok();
    let (trusted, received) = mpsc::unbounded_channel();
    let _trust = AbortOnDrop(tokio::spawn(handle_trust(hub.clone(), received)));
    let _revocations = AbortOnDrop(tokio::spawn(send_revocations(hub.clone())));
    let (save, mut saving) = mpsc::unbounded_channel::<crate::peers::Arrangement>();
    let store = peers.clone();
    let _saver = AbortOnDrop(tokio::spawn(async move {
        while let Some(arrangement) = saving.recv().await {
            if let Err(error) = store.save_arrangement(&arrangement) {
                tracing::warn!(
                    error = format!("{error:#}"),
                    "the screen arrangement could not be saved"
                );
            }
        }
    }));
    let group = share::Group {
        displays: watched,
        control,
        choices: choices.clone(),
        arranged: hub.arranged.clone(),
        saved: peers.arrangement(),
        save,
        reports: reports.clone(),
        locked,
        trust: trusted,
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
    run.await
}

/// What a member starting a link with `key` is told: every system this one
/// trusts, introduced, and every revocation it knows.
fn catch_up(config: &SessionConfig<'_>, key: PublicKey) -> Result<Vec<crate::protocol::Message>> {
    let now = trust::now();
    let mut messages = Vec::new();
    for peer in config.peers.list(now)? {
        if peer.key != key
            && let Some(introduction) = introduction_of(config, peer.key)?
        {
            messages.push(introduction);
        }
    }
    messages.extend(
        config
            .peers
            .revocations()
            .into_iter()
            .map(|revocation| crate::protocol::Message::Revoke { revocation }),
    );
    Ok(messages)
}

/// This system's signed introduction of the peer with `key`, once its
/// signing key is known.
fn introduction_of(config: &SessionConfig<'_>, key: PublicKey) -> Result<Option<crate::protocol::Message>> {
    let Some(peer) = config.peers.trusted(&key, trust::now())? else {
        return Ok(None);
    };
    let Some(signing) = peer.signing else {
        return Ok(None);
    };
    let introduction = crate::introduce::Introduction {
        introducer: config.identity.public_key(),
        newcomer: peer.key,
        newcomer_signing: signing,
        name: peer.name,
        policy: peer.policy,
        trusted_since: peer.paired_at,
    };
    Ok(Some(crate::protocol::Message::Introduce {
        introduction: crate::introduce::Signed::<crate::introduce::Introduction>::new(config.signer, introduction)?,
    }))
}

/// Sides chosen on this system during a session. Without a way to choose,
/// as from the command line, it never changes, and the peer's choices still
/// apply.
fn session_choices(
    arrangement: Option<&watch::Receiver<Option<share::Placing>>>,
) -> watch::Receiver<Option<share::Placing>> {
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
            gate.exchanged(false);
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

    #[tokio::test(start_paused = true)]
    async fn only_one_pairing_exchange_runs_at_a_time() {
        let mut gate = PairingGate::new(Some(Policy::IDLE));
        assert_eq!(gate.for_peer(false), Some(Policy::IDLE));
        assert!(!gate.may_open(), "no second connection while a code is being typed");
        assert_eq!(gate.for_peer(false), None, "a second exchange is refused");
        assert_eq!(gate.for_peer(true), Some(Policy::IDLE), "paired peers still connect");
        gate.exchanged(false);
        assert_eq!(
            gate.for_peer(false),
            Some(Policy::IDLE),
            "the other system may still connect"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_cancelled_pairing_is_not_reopened_at_once() {
        let mut gate = PairingGate::listening(Some(Policy::IDLE), Listening::Closed, false);
        gate.for_peer(false);
        gate.exchanged(false);
        assert!(!gate.may_open(), "the prompt does not come straight back");
        tokio::time::advance(PAIRING_QUIET).await;
        assert!(gate.may_open());
    }

    #[test]
    fn an_always_open_gate_advertises_a_member_once_it_pairs() {
        let mut gate = PairingGate::listening(Some(Policy::IDLE), Listening::Always, false);
        assert!(matches!(gate.offer(), discovery::Offer::Open { member: false, .. }));
        gate.for_peer(false);
        gate.exchanged(true);
        assert!(gate.may_open(), "a paired exchange leaves no pause");
        assert!(matches!(gate.offer(), discovery::Offer::Open { member: true, .. }));
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
            listening: None,
            signer: Box::leak(Box::new(crate::introduce::Signer::generate())),
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

    #[tokio::test]
    async fn each_new_listening_choice_is_seen_once() {
        let (choose, choices) = watch::channel(Listening::Closed);
        let mut choices = Some(choices);
        choose.send_replace(Listening::For(Duration::from_secs(30)));
        assert_eq!(
            listening_changed(&mut choices).await,
            Some(Listening::For(Duration::from_secs(30)))
        );
        // asking again, even for the same, opens it again
        choose.send_replace(Listening::For(Duration::from_secs(30)));
        assert!(listening_changed(&mut choices).await.is_some());
        let waited = tokio::time::timeout(Duration::from_millis(20), listening_changed(&mut choices)).await;
        assert!(waited.is_err(), "no new choice, so nothing changes");
        let waited = tokio::time::timeout(Duration::from_millis(20), listening_changed(&mut None)).await;
        assert!(waited.is_err(), "a session without the choice never changes");
    }

    #[tokio::test(start_paused = true)]
    async fn a_system_with_no_peers_accepts_one_new_system_while_sharing() {
        let mut gate = PairingGate::listening(Some(Policy::IDLE), Listening::Closed, false);
        assert!(gate.is_open());
        assert!(matches!(gate.offer(), discovery::Offer::Open { member: false, .. }));
        tokio::time::advance(PAIRING_WINDOW * 3).await;
        gate.tick();
        assert!(gate.is_open(), "no time limit");
        gate.paired(Trust::NewlyPaired);
        assert!(gate.ended(), "now it is a member");
    }

    #[tokio::test(start_paused = true)]
    async fn a_group_accepts_a_new_system_only_when_asked_or_always_listening() {
        let shut = PairingGate::listening(Some(Policy::IDLE), Listening::Closed, true);
        assert!(!shut.is_open());
        assert_eq!(shut.offer(), discovery::Offer::Closed);

        let mut asked = PairingGate::listening(Some(Policy::IDLE), Listening::For(Duration::from_secs(30)), true);
        assert!(matches!(asked.offer(), discovery::Offer::Open { member: true, .. }));
        tokio::time::advance(Duration::from_secs(30)).await;
        assert!(!asked.is_open());
        assert!(asked.ended());
        asked.close();

        let mut always = PairingGate::listening(Some(Policy::IDLE), Listening::Always, true);
        always.paired(Trust::NewlyPaired);
        assert!(always.is_open(), "stays open after a system pairs");
        for _ in 0..MAX_PAIRING_ATTEMPTS {
            always.for_peer(false);
            always.exchanged(false);
        }
        assert!(!always.is_open());
        assert!(!always.ended(), "out of attempts for now, not closed");
        tokio::time::advance(PAIRING_WINDOW).await;
        always.tick();
        assert!(always.is_open(), "a fresh allowance each window");
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
        let _advertiser = Advertiser::start(&there.identity.public_key(), port, discovery::Offer::Closed).unwrap();
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

    /// A member this system trusts, with the key it signs with.
    fn member(here: &System, name: &str) -> (PublicKey, crate::introduce::Signer) {
        let key = Identity::generate().unwrap().public_key();
        let signer = crate::introduce::Signer::generate();
        here.peers.pin(key, name, Policy::Forever, trust::now()).unwrap();
        here.peers.set_signing(&key, signer.public()).unwrap();
        (key, signer)
    }

    fn introduce(
        signer: &crate::introduce::Signer,
        introducer: PublicKey,
        newcomer: PublicKey,
    ) -> crate::protocol::Message {
        let introduction = crate::introduce::Introduction {
            introducer,
            newcomer,
            newcomer_signing: crate::introduce::Signer::generate().public(),
            name: "Studio".to_owned(),
            policy: Policy::Forever,
            trusted_since: trust::now(),
        };
        crate::protocol::Message::Introduce {
            introduction: crate::introduce::Signed::<crate::introduce::Introduction>::new(signer, introduction)
                .unwrap(),
        }
    }

    fn revoke(signer: &crate::introduce::Signer, by: PublicKey, revoked: PublicKey) -> crate::protocol::Message {
        let revocation = crate::introduce::Revocation {
            by,
            revoked,
            at: trust::now(),
        };
        crate::protocol::Message::Revoke {
            revocation: crate::introduce::Signed::<crate::introduce::Revocation>::new(signer, revocation).unwrap(),
        }
    }

    #[tokio::test]
    async fn a_member_introduces_a_system_with_its_own_signature_only() {
        let here = system();
        let clipboard = watch::channel(true).1;
        let hub = Hub::new(&config(&here, &clipboard));
        let (laptop, laptop_signs) = member(&here, "Laptop");
        let (desk, desk_signs) = member(&here, "Desk");
        let studio = Identity::generate().unwrap().public_key();
        let trusted = |key| here.peers.trusted(&key, trust::now()).unwrap().is_some();

        // signed by someone else
        let forged = introduce(&crate::introduce::Signer::generate(), laptop, studio);
        assert!(act_on_trust(&hub, laptop, forged).is_err());
        // signed by the desk, but in the laptop's name
        assert!(act_on_trust(&hub, desk, introduce(&desk_signs, laptop, studio)).is_err());
        assert!(!trusted(studio));

        act_on_trust(&hub, laptop, introduce(&laptop_signs, laptop, studio)).unwrap();
        assert!(trusted(studio));
        // a member never introduces this system to itself
        act_on_trust(&hub, laptop, introduce(&laptop_signs, laptop, hub.me)).unwrap();
        assert!(!trusted(hub.me));
    }

    #[tokio::test]
    async fn a_revocation_passed_on_by_a_member_is_checked_against_its_maker() {
        let here = system();
        let clipboard = watch::channel(true).1;
        let hub = Hub::new(&config(&here, &clipboard));
        let (laptop, _) = member(&here, "Laptop");
        let (desk, desk_signs) = member(&here, "Desk");
        let (studio, _) = member(&here, "Studio");
        let trusted = |key| here.peers.trusted(&key, trust::now()).unwrap().is_some();

        // the laptop passes on the desk's revocation, but forged
        let forged = revoke(&crate::introduce::Signer::generate(), desk, studio);
        assert!(act_on_trust(&hub, laptop, forged).is_err());
        assert!(trusted(studio));
        // revoking this system is ignored here
        act_on_trust(&hub, laptop, revoke(&desk_signs, desk, hub.me)).unwrap();
        // the desk's own, passed on by the laptop
        act_on_trust(&hub, laptop, revoke(&desk_signs, desk, studio)).unwrap();
        assert!(!trusted(studio));
        assert!(trusted(laptop) && trusted(desk));
    }

    #[test]
    fn a_new_link_hears_of_every_trusted_system_and_revocation() {
        let here = system();
        let clipboard = watch::channel(true).1;
        let config = config(&here, &clipboard);
        let (laptop, laptop_signs) = member(&here, "Laptop");
        let (desk, _) = member(&here, "Desk");
        // a peer whose signing key is not known yet is not introduced
        let quiet = Identity::generate().unwrap().public_key();
        here.peers.pin(quiet, "Quiet", Policy::Forever, trust::now()).unwrap();
        let gone = Identity::generate().unwrap().public_key();
        let crate::protocol::Message::Revoke { revocation } = revoke(&laptop_signs, laptop, gone) else {
            unreachable!()
        };
        here.peers.revoke(revocation, trust::now()).unwrap();

        let messages = catch_up(&config, laptop).unwrap();
        let introduced: Vec<_> = messages
            .iter()
            .filter_map(|message| match message {
                crate::protocol::Message::Introduce { introduction } => Some(introduction.body.newcomer),
                _ => None,
            })
            .collect();
        assert_eq!(
            introduced,
            [desk],
            "neither the link's own peer nor one without a signing key"
        );
        assert!(
            messages
                .iter()
                .any(|message| matches!(message, crate::protocol::Message::Revoke { .. }))
        );
        for message in messages {
            if let crate::protocol::Message::Introduce { introduction } = message {
                introduction.verify(&config.signer.public(), trust::now()).unwrap();
            }
        }
    }

    #[test]
    fn every_new_revocation_is_sent_once_even_when_the_list_is_full() {
        let signer = crate::introduce::Signer::generate();
        let by = Identity::generate().unwrap().public_key();
        let revocation = |at| {
            let crate::protocol::Message::Revoke { mut revocation } =
                revoke(&signer, by, Identity::generate().unwrap().public_key())
            else {
                unreachable!()
            };
            revocation.body.at = at;
            revocation
        };
        let full: Vec<_> = (0..256).map(revocation).collect();
        let mut unsent = Unsent::new(full.clone());
        assert!(unsent.new_ones(full.clone()).is_empty());
        // the oldest falls off as a new one arrives; the count stays 256
        let newer = revocation(999);
        let mut now = full[1..].to_vec();
        now.push(newer.clone());
        assert_eq!(unsent.new_ones(now.clone()), [newer]);
        assert!(unsent.new_ones(now).is_empty());
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
