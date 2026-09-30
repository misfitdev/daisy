//! Connection orchestration shared by the CLI and native menu-bar app.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::Ordering;
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

/// Both peers listen and browse. Paired keys choose one connection opener;
/// anonymous pairing beacons use their random nonces for the same election.
pub async fn automatic<P, O>(config: SessionConfig<'_>, prompt: &mut P, observer: &mut O) -> Result<()>
where
    P: PairingPrompt + Send,
    O: ServiceObserver + Send,
{
    let listener = TcpListener::bind(("0.0.0.0", DEFAULT_PORT)).await?;
    let mut browser = discovery::Browser::start()?;
    let mut pairing = PairingGate::new(config.pairing);
    let mut discoverable = config.discoverable.clone();
    let mut first_seen: HashMap<[u8; discovery::NONCE_LEN], tokio::time::Instant> = HashMap::new();
    loop {
        observer.waiting(
            config.name,
            config.identity.public_key(),
            DEFAULT_PORT,
            config.pairing.filter(|_| pairing.is_open()),
        );
        let advertiser = advertise(&config, DEFAULT_PORT, &pairing, *discoverable.borrow_and_update());
        let mut scan = tokio::time::interval(Duration::from_secs(1));
        loop {
            tokio::select! {
                result = listener.accept() => {
                    let (stream, address) = result?;
                    stream.set_nodelay(true)?;
                    drop(advertiser);
                    match answer(stream, &config, &mut pairing, prompt, observer).await {
                        Ok(peer) => observer.disconnected(&peer),
                        Err(error) => observer.connection_failed(&address.to_string(), &error),
                    }
                    break;
                }
                changed = browser.changed() => {
                    if !changed { anyhow::bail!("Bonjour browsing stopped"); }
                }
                Ok(()) = discoverable.changed() => break,
                _ = scan.tick() => {
                    if pairing.policy.is_some() && !pairing.is_open() && !pairing.closed {
                        pairing.close();
                        observer.pairing_closed();
                        break;
                    }
                    let keys: Vec<_> = config.peers.list(trust::now())?.iter().map(|p| p.key).collect();
                    let own = config.identity.public_key();
                    let mut found = browser.current(&keys);
                    found.sort_by_key(|heard| heard.election);
                    let now = tokio::time::Instant::now();
                    first_seen.retain(|election, _| found.iter().any(|heard| heard.election == *election));
                    for heard in &found { first_seen.entry(heard.election).or_insert(now); }
                    let election = advertiser.as_ref().map(|a| a.election);
                    let candidate = found.into_iter().find(|heard| {
                        let seen_for = now - first_seen[&heard.election];
                        discovery::opens_connection(own, election, heard, pairing.is_open(), seen_for)
                    });
                    if let Some(heard) = candidate {
                        // wait out the grace again, so two systems that both
                        // connected do not keep colliding
                        first_seen.remove(&heard.election);
                        let mut expected = match heard.seen { discovery::Seen::Paired(key) => Some(key), discovery::Seen::Pairing => None };
                        let attempt = SessionConfig { pairing: pairing.for_peer(expected.is_some()), ..config };
                        let address = heard.address.to_string();
                        observer.connecting(&address, peer_name(config.peers, expected).as_deref());
                        drop(advertiser);
                        let (_, result) = connect_once(attempt, &address, &mut expected, prompt, observer).await;
                        if expected.is_some() { pairing.completed = true; }
                        if let Err(error) = result { observer.connection_failed(&address, &error); }
                        tokio::time::sleep(Duration::from_millis(500)).await;
                        break;
                    }
                }
            }
        }
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
    /// Also apply `side` to a peer paired before, as a new choice.
    pub choose_side: bool,
    /// Whether this system shares its clipboard; may change mid-session.
    pub clipboard: &'a watch::Receiver<bool>,
    /// Whether a waiting system advertises itself with Bonjour.
    pub discoverable: &'a watch::Receiver<bool>,
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

/// Wait for peers to connect until the future is cancelled.
pub async fn listen<P, O>(
    config: SessionConfig<'_>,
    bind: &str,
    port: u16,
    prompt: &mut P,
    observer: &mut O,
) -> Result<()>
where
    P: PairingPrompt + Send,
    O: ServiceObserver + Send,
{
    let listener = TcpListener::bind((bind, port))
        .await
        .with_context(|| format!("listening on {bind}:{port}"))?;
    observer.waiting(config.name, config.identity.public_key(), port, config.pairing);

    let mut pairing_gate = PairingGate::new(config.pairing);
    let mut discoverable = config.discoverable.clone();
    loop {
        // advertise only while waiting, and only while allowed
        let _advertiser = advertise(&config, port, &pairing_gate, *discoverable.borrow_and_update());
        let accepted = if let Some(deadline) = pairing_gate.deadline() {
            tokio::select! {
                result = listener.accept() => Some(result),
                () = tokio::time::sleep_until(deadline) => None,
                Ok(()) = discoverable.changed() => continue,
            }
        } else {
            tokio::select! {
                result = listener.accept() => Some(result),
                Ok(()) = discoverable.changed() => continue,
            }
        };
        drop(_advertiser);
        let Some(accepted) = accepted else {
            pairing_gate.close();
            observer.pairing_closed();
            continue;
        };
        let (stream, address) = accepted?;
        stream.set_nodelay(true)?;
        let pairing_was_open = pairing_gate.is_open();
        match answer(stream, &config, &mut pairing_gate, prompt, observer).await {
            Ok(peer) => observer.disconnected(&peer),
            Err(error) => observer.connection_failed(&address.to_string(), &error),
        }
        if pairing_was_open && !pairing_gate.is_open() {
            observer.pairing_closed();
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
                let (lasted, result) = connect_once(attempt, &address, &mut expected, prompt, observer).await;
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
        let session = run_session(
            channel,
            config.peers,
            &peer,
            config.choose_side.then_some(config.side),
            config.clipboard,
            observer,
        )
        .await;
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
    pairing: &mut PairingGate,
    prompt: &mut P,
    observer: &mut O,
) -> Result<String>
where
    P: PairingPrompt + Send,
    O: ServiceObserver + Send,
{
    let mut channel = tokio::time::timeout(HANDSHAKE_TIMEOUT, Channel::respond(stream, config.identity))
        .await
        .context("the Noise handshake timed out")??;
    let known = config.peers.trusted(&channel.remote_key(), trust::now())?.is_some();
    let policy = pairing.for_peer(known);
    let (peer, trust_status) = tokio::time::timeout(
        TRUST_TIMEOUT,
        settle_trust(&mut channel, config.peers, config.name, policy, prompt, observer),
    )
    .await
    .context("pairing or trust negotiation timed out")??;
    let pairing_was_open = pairing.is_open();
    if trust_status == Trust::NewlyPaired {
        config
            .peers
            .agree_side(&channel.remote_key(), config.side, trust::now())?;
    }
    pairing.paired(trust_status);
    if pairing_was_open && !pairing.is_open() {
        observer.pairing_closed();
    }
    run_session(
        channel,
        config.peers,
        &peer,
        config.choose_side.then_some(config.side),
        config.clipboard,
        observer,
    )
    .await?;
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

async fn run_session<S, O>(
    mut channel: Channel<S>,
    peers: &PeerStore,
    peer: &str,
    chosen_side: Option<Side>,
    clipboard: &watch::Receiver<bool>,
    observer: &mut O,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    O: ServiceObserver + Send,
{
    let screen = macos::screen_bounds()?;
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
    let mut sharing = Sharing::new(Pasteboard, clipboard.clone());
    let mut visit = peers.visit(key)?;
    observer.connected(peer, key, side);
    let control = std::sync::Arc::new(crate::control::SharedControl::new(initiator));
    let mut link = control.watch_link();
    let mut injector = Injector::new();
    let (messages, input) = mpsc::channel(INPUT_QUEUE_CAPACITY);
    let (mut capture, overflowed) = Capture::start(screen, side, messages, control.clone())?;
    let until = async {
        tokio::select! {
            error = peers.watch(key, peer) => error,
            _ = async {
                loop {
                    if overflowed.load(Ordering::Acquire) { break; }
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            } => anyhow::anyhow!("local input queue overloaded; control was reclaimed"),
        }
    };
    let session = share::together(
        channel,
        share::SharedLayout { screen, side, control },
        input,
        &mut capture,
        &mut injector,
        &mut sharing,
        until,
    );
    tokio::pin!(session);
    let result = loop {
        tokio::select! {
            result = &mut session => break result,
            Ok(()) = link.changed() => {
                let current = *link.borrow_and_update();
                observer.link(peer, current);
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
        let mut gate = PairingGate::new(Some(Policy::Idle));
        assert!(gate.is_open());
        for _ in 0..MAX_PAIRING_ATTEMPTS {
            assert_eq!(gate.for_peer(false), Some(Policy::Idle));
        }
        assert!(!gate.is_open());
        assert_eq!(gate.for_peer(false), None);
        assert_eq!(gate.for_peer(true), Some(Policy::Idle));

        let mut gate = PairingGate::new(Some(Policy::Idle));
        gate.paired(Trust::NewlyPaired);
        assert!(!gate.is_open());
        assert_eq!(gate.for_peer(false), None);

        let mut gate = PairingGate::new(Some(Policy::Idle));
        tokio::time::advance(PAIRING_WINDOW).await;
        assert!(!gate.is_open());
        assert_eq!(gate.for_peer(false), None);

        let mut gate = PairingGate::new(Some(Policy::Idle));
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
            choose_side: false,
            clipboard,
            discoverable,
        }
    }

    #[test]
    fn only_the_first_connection_may_pair() {
        assert_eq!(pairing_for_attempt(Some(Policy::Idle), false), Some(Policy::Idle));
        assert_eq!(pairing_for_attempt(Some(Policy::Idle), true), None);
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
            .pin(there.identity.public_key(), "Studio", Policy::Idle, now)
            .unwrap();
        there
            .peers
            .pin(here.identity.public_key(), "Laptop", Policy::Idle, now)
            .unwrap();
        let clipboard = watch::channel(true).1;
        let mut observer = Recorded::default();
        // A trusted peer closes during layout agreement, then accepts again.
        let server = async {
            let mut sessions = 0;
            while sessions < 2 {
                let (stream, _) = listener.accept().await.unwrap();
                let mut channel = Channel::respond(stream, &there.identity).await.unwrap();
                establish_trust(&mut channel, &there.peers, "Studio", false, Policy::Idle, &mut NoCodes)
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
            .pin(there.identity.public_key(), "Studio", Policy::Idle, now)
            .unwrap();
        there
            .peers
            .pin(here.identity.public_key(), "Laptop", Policy::Idle, now)
            .unwrap();
        let _advertiser = Advertiser::start(&there.identity.public_key(), port, false).unwrap();
        let server = async {
            let (stream, _) = listener.accept().await.unwrap();
            let mut channel = Channel::respond(stream, &there.identity).await.unwrap();
            establish_trust(&mut channel, &there.peers, "Studio", false, Policy::Idle, &mut NoCodes)
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
            let _ = establish_trust(&mut channel, &there.peers, "Studio", false, Policy::Idle, &mut NoCodes).await;
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
        let client = connect_once(
            config(&here, &clipboard),
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
