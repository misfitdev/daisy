//! Connection orchestration shared by the CLI and native menu-bar app.

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

/// How long reaching the other Mac may take before it counts as unreachable;
/// a sleeping Mac otherwise holds a connection attempt for over a minute.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(15);
const TRUST_TIMEOUT: Duration = Duration::from_secs(120);
const PAIRING_WINDOW: Duration = Duration::from_secs(10 * 60);
const MAX_PAIRING_ATTEMPTS: u8 = 5;
const INPUT_QUEUE_CAPACITY: usize = 1024;

type Pairing = Option<Policy>;

/// Receives human-readable state changes without taking part in protocol logic.
pub trait ServiceObserver {
    fn waiting(&mut self, _name: &str, _key: PublicKey, _port: u16, _pairing: Pairing) {}
    fn connecting(&mut self, _address: &str) {}
    /// The connection could not be made or was lost; trying again after `wait`.
    fn reconnecting(&mut self, _address: &str, _wait: Duration) {}
    fn paired(&mut self, _peer: &str, _key: PublicKey, _policy: Policy) {}
    fn connected(&mut self, _peer: &str, _key: PublicKey, _drive: Option<Side>) {}
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
    pub drive: Option<Side>,
    /// Whether this Mac shares its clipboard; may change mid-session.
    pub clipboard: &'a watch::Receiver<bool>,
    /// Whether a waiting Mac advertises itself with Bonjour.
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

/// Wait for Macs to connect until the future is cancelled.
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

/// Connect to the Mac at `address` and keep the session going: once a session
/// has run, reconnect and run the handshake again whenever it drops. The
/// first connection is not retried, so a wrong address or a Mac not set up
/// yet is reported. Stops on anything a retry cannot fix: trust ended, a
/// different Mac answering, or a local or setup error. Only the first
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
    // a Mac chosen from those found on the network is already paired: check its key from the start
    let mut expected = peer;
    let mut started = false;
    let mut waits = reconnect::waits();
    loop {
        let attempt = SessionConfig {
            pairing: pairing_for_attempt(config.pairing, expected.is_some()),
            ..config
        };
        let result = match locate(expected, address).await {
            Ok(address) => {
                observer.connecting(&address);
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
            // the other Mac ended the session; wait for it to come back
            Ok(()) => {}
            // Until a session has worked, a failure is more likely a wrong
            // address or a Mac not set up yet than a network blip: say so.
            Err(error) if !started => return Err(error),
            Err(error) if reconnect::retryable(&error) => {
                tracing::info!(error = format!("{error:#}"), "connection lost; reconnecting");
            }
            Err(error) => return Err(error),
        }
        let wait = waits.next().unwrap_or(Duration::from_secs(10));
        observer.reconnecting(address, wait);
        tokio::time::sleep(wait).await;
    }
}

/// This Mac's Bonjour advertisement while waiting, if allowed. A failure to
/// advertise is logged, not fatal: connecting by name or address still works.
fn advertise(config: &SessionConfig<'_>, port: u16, pairing: &PairingGate, allowed: bool) -> Option<Advertiser> {
    if !allowed {
        return None;
    }
    Advertiser::start(&config.identity.public_key(), port, pairing.is_open())
        .inspect_err(|error| tracing::warn!(error = format!("{error:#}"), "could not advertise with Bonjour"))
        .ok()
}

/// How long each attempt looks for a paired Mac on the network before
/// falling back to the saved address.
const FIND_TIMEOUT: Duration = Duration::from_secs(3);

/// Where to reach the Mac this attempt: found with Bonjour when its key is
/// known, otherwise the saved name or address.
async fn locate(peer: Option<PublicKey>, saved: &str) -> Result<String> {
    if let Some(key) = peer
        && let Some(found) = discovery::find(key, FIND_TIMEOUT).await
    {
        return Ok(found.to_string());
    }
    if saved.trim().is_empty() {
        return Err(Unreachable {
            address: "the paired Mac".to_owned(),
            source: std::io::ErrorKind::NotFound.into(),
        }
        .into());
    }
    Ok(connect_address(saved))
}

/// Pairing is offered only until a session has started; reconnecting to that
/// Mac never pairs, so a stranger at the same address cannot slip in.
fn pairing_for_attempt(pairing: Pairing, reconnecting: bool) -> Pairing {
    if reconnecting { None } else { pairing }
}

/// One connection: reach the Mac, run the Noise handshake, settle trust, and
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
        let (peer, _) = tokio::time::timeout(
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
        *expected = Some(channel.remote_key());
        let started = tokio::time::Instant::now();
        let session = run_session(channel, config.peers, &peer, config.drive, config.clipboard, observer).await;
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
    pairing.paired(trust_status);
    if pairing_was_open && !pairing.is_open() {
        observer.pairing_closed();
    }
    run_session(channel, config.peers, &peer, config.drive, config.clipboard, observer).await?;
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
    channel: Channel<S>,
    peers: &PeerStore,
    peer: &str,
    drive: Option<Side>,
    clipboard: &watch::Receiver<bool>,
    observer: &mut O,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
    O: ServiceObserver + Send,
{
    let screen = macos::screen_bounds()?;
    let mut sharing = Sharing::new(Pasteboard, clipboard.clone());
    let key = channel.remote_key();
    let mut visit = peers.visit(key)?;
    observer.connected(peer, key, drive);

    let result = match drive {
        Some(side) => {
            let (messages, input) = mpsc::channel(INPUT_QUEUE_CAPACITY);
            let (mut capture, overflowed) = Capture::start(screen, side, messages)?;
            let until = async {
                tokio::select! {
                    error = peers.watch(key, peer) => error,
                    _ = async {
                        loop {
                            if overflowed.load(Ordering::Acquire) {
                                break;
                            }
                            tokio::time::sleep(Duration::from_millis(10)).await;
                        }
                    } => anyhow::anyhow!("local input queue overloaded; control was reclaimed"),
                }
            };
            share::drive(channel, side, input, &mut capture, &mut sharing, until).await
        }
        None => {
            share::follow(
                channel,
                screen,
                &mut Injector::new(),
                &mut sharing,
                peers.watch(key, peer),
            )
            .await
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
    }

    impl ServiceObserver for Recorded {
        fn reconnecting(&mut self, _address: &str, wait: Duration) {
            self.waits.push(wait);
        }
    }

    struct NoCodes;

    impl PairingPrompt for NoCodes {
        fn show_code(&mut self, _code: &crate::pairing::PairingCode, _peer: &str) {}
        async fn ask_code(&mut self, _peer: &str) -> Result<crate::pairing::PairingCode> {
            anyhow::bail!("no code")
        }
    }

    struct Mac {
        _home: tempfile::TempDir,
        identity: Identity,
        peers: PeerStore,
    }

    fn mac() -> Mac {
        let home = tempfile::tempdir().unwrap();
        let peers = PeerStore::open(home.path()).unwrap();
        Mac {
            identity: Identity::generate().unwrap(),
            peers,
            _home: home,
        }
    }

    fn config<'a>(mac: &'a Mac, clipboard: &'a watch::Receiver<bool>) -> SessionConfig<'a> {
        let discoverable: &'a watch::Receiver<bool> = Box::leak(Box::new(watch::channel(false).1));
        SessionConfig {
            identity: &mac.identity,
            peers: &mac.peers,
            name: "Laptop",
            pairing: None,
            drive: None,
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
    async fn a_mac_that_was_never_reached_is_reported_not_retried() {
        let mut prompt = NoCodes;
        // a port that was just free: nothing is listening on it
        let port = TcpListener::bind("127.0.0.1:0")
            .await
            .unwrap()
            .local_addr()
            .unwrap()
            .port();
        let here = mac();
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
    async fn a_session_that_drops_is_reconnected() {
        let mut prompt = NoCodes;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let (here, there) = (mac(), mac());
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
        // the other Mac takes the Host role and hangs up; then takes the next connection
        let server = async {
            let mut sessions = 0;
            while sessions < 2 {
                let (stream, _) = listener.accept().await.unwrap();
                let mut channel = Channel::respond(stream, &there.identity).await.unwrap();
                establish_trust(&mut channel, &there.peers, "Studio", false, Policy::Idle, &mut NoCodes)
                    .await
                    .unwrap();
                channel
                    .send(&crate::protocol::Message::Drive { side: Side::Left })
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
        let (sessions, _) = tokio::join!(server, client);
        assert_eq!(sessions, 2);
        assert!(!observer.waits.is_empty());
    }

    // Uses the real network stack: the waiting Mac advertises with Bonjour
    // and the other finds it from its key alone, with no address.
    #[tokio::test]
    #[ignore = "uses the local network; run by hand with --ignored"]
    async fn a_paired_mac_is_found_and_connected_without_an_address() {
        let mut prompt = NoCodes;
        let listener = TcpListener::bind("0.0.0.0:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let (here, there) = (mac(), mac());
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
            .expect("the paired Mac was never reached");
        assert_eq!(peer, "Laptop");
    }

    #[tokio::test]
    async fn a_mac_that_no_longer_trusts_this_one_stops_the_retries() {
        let mut prompt = NoCodes;
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let there = mac();
        let server = async {
            let (stream, _) = listener.accept().await.unwrap();
            let mut channel = Channel::respond(stream, &there.identity).await.unwrap();
            let _ = establish_trust(&mut channel, &there.peers, "Studio", false, Policy::Idle, &mut NoCodes).await;
        };
        let here = mac();
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
    async fn a_different_mac_at_the_address_is_refused() {
        let mut prompt = NoCodes;
        let mut observer = Recorded::default();
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap().to_string();
        let impostor = mac();
        let server = async {
            let (stream, _) = listener.accept().await.unwrap();
            let _ = Channel::respond(stream, &impostor.identity).await;
        };
        let here = mac();
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
