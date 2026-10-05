//! Background controller for the native menu-bar application.
//!
//! AppKit stays on the main thread. This module owns the Tokio runtime and
//! communicates with it through typed channels, so no native callback waits on
//! network or disk work.

use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{ErrorKind, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc as tokio_mpsc, oneshot, watch};

use crate::identity::{Identity, PublicKey};
use crate::input::Side;
use crate::pairing::{PairingCode, PairingPrompt};
use crate::peers::{Peer, PeerStore};
use crate::service::{self, Listening, ServiceObserver};
use crate::session::{Mismatch, SessionError};
use crate::trust::{self, Policy};

const SETTINGS_FILE: &str = "settings.toml";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum Connection {
    #[default]
    Automatic,
    Connect {
        address: String,
        /// The peer's key when it was picked from those found on the
        /// network; each attempt then finds it wherever it is now.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        peer: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSettings {
    #[serde(flatten)]
    pub connection: Connection,
    /// Where the peer sits relative to this system.
    pub side: Side,
    #[serde(default)]
    pub trust: Policy,
}

impl Default for SessionSettings {
    fn default() -> Self {
        Self {
            connection: Connection::default(),
            side: Side::Right,
            trust: Policy::default(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppSettings {
    #[serde(default)]
    pub last_session: SessionSettings,
    /// Send and receive the clipboard when control crosses.
    #[serde(default = "default_share_clipboard")]
    pub share_clipboard: bool,
    /// Accept a new system whenever sharing, not only when asked.
    #[serde(default)]
    pub always_discoverable: bool,
    /// Sharing was on when Daisy last ran, so it starts again.
    #[serde(default)]
    pub sharing: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            last_session: SessionSettings::default(),
            share_clipboard: default_share_clipboard(),
            always_discoverable: false,
            sharing: false,
        }
    }
}

fn default_share_clipboard() -> bool {
    true
}

#[derive(Debug)]
pub enum Command {
    Start {
        settings: SessionSettings,
        /// The screen edge was chosen for this start, rather than left as saved.
        side_chosen: bool,
    },
    /// Connect one address without replacing a running group.
    ConnectByAddress(SessionSettings),
    Stop,
    Forget {
        selector: String,
    },
    SetTrust {
        selector: String,
        policy: Policy,
    },
    Refresh,
    /// Turn clipboard sharing on or off, including for a running session.
    SetClipboard(bool),
    /// Accept a new system for `ADD_SYSTEM_WINDOW`.
    AddSystem,
    /// Stop accepting a new system early.
    CancelAdding,
    /// Accept a new system whenever sharing.
    SetAlwaysDiscoverable(bool),
    /// Put a member's displays where a person dropped them in the group.
    Place(PublicKey, crate::layout::Offset),
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Idle,
    Starting,
    Waiting {
        port: u16,
        pairing: bool,
        /// The one paired peer, when there is exactly one to wait for.
        looking_for: Option<String>,
    },
    Connecting {
        address: String,
        peer: Option<String>,
    },
    /// The connection was lost or could not be made; trying again in `wait`.
    Reconnecting {
        address: String,
        peer: Option<String>,
        wait: std::time::Duration,
    },
    /// Linked with these members, by name, in the order they joined.
    Connected {
        peers: Vec<(String, PublicKey)>,
    },
    Problem {
        summary: String,
        recovery: String,
    },
}

pub enum Event {
    Ready {
        settings: AppSettings,
        peers: Vec<Peer>,
        first_run: bool,
        /// This system's key.
        me: PublicKey,
        /// Whether a configuration profile allows Always Discoverable.
        always_discoverable_allowed: bool,
    },
    Status(Status),
    /// Whether the running group is open to a new system.
    Adding(bool),
    Peers(Vec<Peer>),
    ShowPairingCode {
        peer: String,
        code: PairingCode,
    },
    AskPairingCode {
        peer: String,
        reply: oneshot::Sender<String>,
    },
    /// A new peer was paired and trusted with the saved default.
    Paired {
        peer: String,
        key: PublicKey,
    },
    /// A pairing ended because the code typed did not match.
    CodeMismatch,
    Notice {
        title: String,
        detail: String,
    },
    /// How one peer's link is doing.
    Link {
        key: PublicKey,
        link: crate::control::Link,
    },
    /// Where every member's displays sit, while the group runs.
    Arranged(crate::share::Layout),
}

pub struct Handle {
    commands: tokio_mpsc::UnboundedSender<Command>,
    events: Receiver<Event>,
}

impl Handle {
    pub fn send(&self, command: Command) -> Result<()> {
        self.commands
            .send(command)
            .map_err(|_| anyhow::anyhow!("Daisy's background controller stopped"))
    }

    pub fn try_recv(&self) -> Result<Option<Event>, mpsc::TryRecvError> {
        match self.events.try_recv() {
            Ok(event) => Ok(Some(event)),
            Err(mpsc::TryRecvError::Empty) => Ok(None),
            Err(error) => Err(error),
        }
    }
}

pub fn spawn(home: PathBuf, name: String) -> Result<Handle> {
    let (command_tx, command_rx) = tokio_mpsc::unbounded_channel();
    let (event_tx, event_rx) = mpsc::channel();
    thread::Builder::new()
        .name("daisy-controller".to_owned())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_multi_thread().enable_all().build();
            match runtime {
                Ok(runtime) => runtime.block_on(run(home, name, command_rx, event_tx)),
                Err(error) => {
                    let _ = event_tx.send(Event::Status(Status::Problem {
                        summary: "Daisy could not start".to_owned(),
                        recovery: format!("Restart Daisy. {error}"),
                    }));
                }
            }
        })
        .context("starting Daisy's background controller")?;
    Ok(Handle {
        commands: command_tx,
        events: event_rx,
    })
}

async fn run(home: PathBuf, name: String, mut commands: tokio_mpsc::UnboundedReceiver<Command>, events: Sender<Event>) {
    let first_run = !home.join(SETTINGS_FILE).exists();
    let mut settings = load_settings(&home).unwrap_or_else(|error| {
        tracing::error!(error = ?error, "saved setup could not be read");
        let _ = send_problem(
            &events,
            "Saved setup could not be read",
            "Open Daisy and save the setup again.".to_owned(),
        );
        AppSettings::default()
    });
    let peers = list_peers(&home).unwrap_or_else(|error| {
        tracing::error!(error = ?error, "paired peers could not be read");
        let _ = send_problem(
            &events,
            "Paired peers could not be read",
            "Check Daisy's data-folder permissions, then refresh.".to_owned(),
        );
        Vec::new()
    });
    let arranged = match &settings.last_session.connection {
        Connection::Connect { peer: Some(key), .. } => peers.iter().find(|p| p.key.to_hex() == *key),
        _ if peers.len() == 1 => peers.first(),
        _ => None,
    };
    if let Some(side) = arranged.map(|peer| peer.side) {
        settings.last_session.side = side;
    }
    let mut stored = settings.clone();
    let (share_clipboard, clipboard) = watch::channel(stored.share_clipboard);
    // always advertised while sharing; the advertisement names no one
    let (_discoverable, discoverable) = watch::channel(true);
    let (arrange, arrangement) = watch::channel(None);
    let base_listening = |stored: &AppSettings| {
        listening(
            stored.always_discoverable,
            crate::macos::managed::always_discoverable_allowed(),
        )
    };
    let (listen, listening) = watch::channel(base_listening(&stored));
    let me = match Identity::load_or_create(&home.join("identity")) {
        Ok(identity) => identity.public_key(),
        Err(error) => {
            let _ = send_problem(&events, "Daisy's key could not be read", format!("{error:#}"));
            return;
        }
    };
    if events
        .send(Event::Ready {
            settings,
            peers,
            first_run,
            me,
            always_discoverable_allowed: crate::macos::managed::always_discoverable_allowed(),
        })
        .is_err()
    {
        return;
    }

    let mut session: Option<tokio::task::JoinHandle<()>> = None;
    let mut addresses: Option<tokio_mpsc::UnboundedSender<(String, Option<PublicKey>)>> = None;
    while let Some(command) = commands.recv().await {
        let command = match command {
            Command::ConnectByAddress(settings) => {
                let active = session.as_ref().is_some_and(|running| !running.is_finished());
                if address_action(active) == AddressAction::Join {
                    stored.last_session = settings.clone();
                    if let Err(error) = save_settings(&home, &stored) {
                        tracing::error!(error = ?error, "setup could not be saved");
                        let _ = send_problem(&events, "Setup could not be saved", error.to_string());
                        continue;
                    }
                    if let Some(sender) = &addresses
                        && let Some(address) = explicit_connection(&settings.connection)
                        && sender.send(address).is_ok()
                    {
                        continue;
                    }
                    // The group may have ended between checking its task and sending.
                    // Start a new one rather than silently losing this address.
                }
                Command::Start {
                    settings,
                    side_chosen: false,
                }
            }
            other => other,
        };
        match command {
            Command::Start { settings, side_chosen } => {
                if let Some(running) = session.take() {
                    running.abort();
                }
                stored.last_session = settings.clone();
                stored.sharing = true;
                if let Err(error) = save_settings(&home, &stored) {
                    tracing::error!(error = ?error, "setup could not be saved");
                    let _ = send_problem(
                        &events,
                        "Setup could not be saved",
                        "Check that Daisy can write its data folder, then try again.".to_owned(),
                    );
                    continue;
                }
                let _ = events.send(Event::Status(Status::Starting));
                let session_home = home.clone();
                let session_name = name.clone();
                let session_events = events.clone();
                listen.send_replace(base_listening(&stored));
                let (address_sender, address_receiver) = tokio_mpsc::unbounded_channel();
                let (settings, address) = group_start(settings);
                if let Some(address) = address {
                    let _ = address_sender.send(address);
                }
                addresses = Some(address_sender);
                let live = Live {
                    addresses: address_receiver,
                    clipboard: clipboard.clone(),
                    discoverable: discoverable.clone(),
                    arrangement: arrangement.clone(),
                    listening: listening.clone(),
                };
                session = Some(tokio::spawn(async move {
                    let start = Start { settings, side_chosen };
                    run_session(session_home, session_name, start, live, session_events).await;
                }));
            }
            Command::ConnectByAddress(_) => unreachable!("address command was normalized"),
            Command::Stop => {
                if let Some(running) = session.take() {
                    running.abort();
                }
                stored.sharing = false;
                if let Err(error) = save_settings(&home, &stored) {
                    tracing::error!(error = ?error, "sharing state could not be saved");
                }
                let _ = events.send(Event::Status(Status::Idle));
            }
            Command::Forget { selector } => {
                match forget_peer(&home, &selector) {
                    Ok(peers) => {
                        // A matching session observes the peer-store change and
                        // exits through its normal cleanup path. A session with
                        // another peer continues uninterrupted.
                        let _ = events.send(Event::Peers(peers));
                    }
                    Err(error) => {
                        tracing::error!(error = ?error, "paired peer could not be forgotten");
                        let _ = send_problem(
                            &events,
                            "That peer could not be forgotten",
                            "Refresh peers and try again.".to_owned(),
                        );
                    }
                }
            }
            Command::SetTrust { selector, policy } => match set_trust(&home, &selector, policy) {
                Ok((_, peers)) => {
                    let _ = events.send(Event::Peers(peers));
                }
                Err(error) => {
                    tracing::error!(error = ?error, "trust policy could not be changed");
                    let _ = send_problem(
                        &events,
                        "Trust could not be changed",
                        "Refresh peers and try again.".to_owned(),
                    );
                }
            },
            Command::Refresh => match list_peers(&home) {
                Ok(peers) => {
                    let _ = events.send(Event::Peers(peers));
                }
                Err(error) => {
                    tracing::error!(error = ?error, "paired peers could not be refreshed");
                    let _ = send_problem(
                        &events,
                        "Paired peers could not be refreshed",
                        "Check Daisy's data folder and try again.".to_owned(),
                    );
                }
            },
            Command::SetClipboard(on) => {
                stored.share_clipboard = on;
                share_clipboard.send_replace(on);
                if let Err(error) = save_settings(&home, &stored) {
                    tracing::error!(error = ?error, "clipboard setting could not be saved");
                    let _ = send_problem(
                        &events,
                        "Clipboard setting could not be saved",
                        "Check that Daisy can write its data folder, then try again.".to_owned(),
                    );
                }
            }
            Command::AddSystem => {
                listen.send_replace(Listening::For(ADD_SYSTEM_WINDOW));
            }
            Command::CancelAdding => {
                listen.send_replace(base_listening(&stored));
            }
            Command::SetAlwaysDiscoverable(on) => {
                if on && !crate::macos::managed::always_discoverable_allowed() {
                    continue;
                }
                stored.always_discoverable = on;
                listen.send_replace(base_listening(&stored));
                if let Err(error) = save_settings(&home, &stored) {
                    tracing::error!(error = ?error, "Always Discoverable could not be saved");
                    let _ = send_problem(
                        &events,
                        "Always Discoverable could not be saved",
                        "Check that Daisy can write its data folder, then try again.".to_owned(),
                    );
                }
            }
            Command::Place(key, offset) => {
                arrange.send_replace(Some((key, offset)));
            }
            Command::Shutdown => {
                if let Some(running) = session.take() {
                    running.abort();
                }
                break;
            }
        }
    }
}

/// Decide whether an address joins the running group or starts a new one.
#[derive(Debug, PartialEq, Eq)]
enum AddressAction {
    Join,
    Start,
}

fn address_action(active: bool) -> AddressAction {
    if active {
        AddressAction::Join
    } else {
        AddressAction::Start
    }
}

type ExplicitAddress = (String, Option<PublicKey>);

/// An address seeds the group; it never switches off listening or discovery.
fn group_start(mut settings: SessionSettings) -> (SessionSettings, Option<ExplicitAddress>) {
    let address = explicit_connection(&settings.connection);
    settings.connection = Connection::Automatic;
    (settings, address)
}

fn explicit_connection(connection: &Connection) -> Option<(String, Option<PublicKey>)> {
    match connection {
        Connection::Automatic => None,
        Connection::Connect { address, peer } => Some((address.clone(), peer.as_deref().and_then(PublicKey::from_hex))),
    }
}

/// One press of Start, as `Command::Start` carries it.
struct Start {
    settings: SessionSettings,
    side_chosen: bool,
}

/// When a running group accepts a new system, unless Add a System asks:
/// always only if chosen here and allowed by any configuration profile.
fn listening(always_discoverable: bool, allowed: bool) -> Listening {
    if always_discoverable && allowed {
        Listening::Always
    } else {
        Listening::Closed
    }
}

/// How long Add a System accepts a new system.
pub const ADD_SYSTEM_WINDOW: std::time::Duration = std::time::Duration::from_secs(30);

/// What may change while a session runs.
struct Live {
    addresses: tokio_mpsc::UnboundedReceiver<(String, Option<PublicKey>)>,
    clipboard: watch::Receiver<bool>,
    discoverable: watch::Receiver<bool>,
    arrangement: watch::Receiver<Option<crate::share::Placing>>,
    listening: watch::Receiver<Listening>,
}

async fn run_session(home: PathBuf, name: String, start: Start, live: Live, events: Sender<Event>) {
    let Live {
        addresses,
        clipboard,
        discoverable,
        arrangement,
        mut listening,
    } = live;
    // the session starts from the current choice; only later ones are news
    listening.mark_unchanged();
    let Start { settings, side_chosen } = start;
    let result = async {
        let identity = Identity::load_or_create(&home.join("identity"))?;
        let signer = crate::introduce::Signer::load_or_create(&home.join("device-identity"))?;
        let peers = PeerStore::open(&home)?;
        let choose_side = AtomicBool::new(side_chosen);
        let mut prompt = ControllerPrompt { events: events.clone() };
        let mut observer = ControllerObserver {
            told_version: None,
            events: events.clone(),
            home: home.clone(),
            waiting: None,
            stats: std::collections::BTreeMap::new(),
            linked: Vec::new(),
            adding: false,
        };
        let pairing = Some(settings.trust);
        let config = service::SessionConfig {
            identity: &identity,
            peers: &peers,
            name: &name,
            pairing,
            side: settings.side,
            choose_side: &choose_side,
            clipboard: &clipboard,
            discoverable: &discoverable,
            arrangement: Some(&arrangement),
            listening: Some(&listening),
            signer: &signer,
        };
        service::automatic_with_addresses(config, Some(addresses), &mut prompt, &mut observer).await
    }
    .await;

    match result {
        Ok(()) => {
            let _ = events.send(Event::Status(Status::Idle));
        }
        Err(error) => {
            tracing::error!(error = ?error, "connection session stopped");
            let _ = match version_copy(&home, &error) {
                Some((title, detail)) => send_problem(&events, &title, detail),
                None => send_problem(&events, "Connection stopped", recovery_for(&error)),
            };
        }
    }
}

#[derive(Clone)]
struct ControllerPrompt {
    events: Sender<Event>,
}

impl PairingPrompt for ControllerPrompt {
    fn show_code(&mut self, code: &PairingCode, peer: &str) {
        let _ = self.events.send(Event::ShowPairingCode {
            peer: peer.to_owned(),
            code: *code,
        });
    }

    fn ask_code(&mut self, peer: &str) -> impl std::future::Future<Output = Result<PairingCode>> + Send {
        let (reply, answer) = oneshot::channel();
        let sent = self.events.send(Event::AskPairingCode {
            peer: peer.to_owned(),
            reply,
        });
        async move {
            sent.context("showing pairing-code prompt")?;
            let text = answer.await.context("pairing-code prompt closed")?;
            PairingCode::parse(text.trim()).context("enter the six-digit code shown on the other system")
        }
    }
}

struct ControllerObserver {
    /// The version mismatch last reported, so a retry does not repeat it.
    told_version: Option<(String, String)>,
    events: Sender<Event>,
    home: PathBuf,
    waiting: Option<(u16, bool)>,
    /// Each running link's round trips, kept for `daisy stats`.
    stats: std::collections::BTreeMap<PublicKey, crate::latency::LinkStats>,
    /// The members with a running link, in the order they joined, and how
    /// many links each has: a second connection briefly overlaps the first.
    linked: Vec<(String, PublicKey, usize)>,
    /// Whether pairing was opened while the group runs.
    adding: bool,
}

impl ControllerObserver {
    fn send_connected(&self) {
        let peers = self.linked.iter().map(|(name, key, _)| (name.clone(), *key)).collect();
        let _ = self.events.send(Event::Status(Status::Connected { peers }));
    }

    fn save_stats(&self) {
        let links: Vec<_> = self.stats.values().cloned().collect();
        if let Err(error) = crate::latency::save(&self.home, &links) {
            tracing::debug!(error = format!("{error:#}"), "round trips could not be saved");
        }
    }
}

impl ControllerObserver {
    fn send_waiting(&self, port: u16, pairing: bool) {
        let peers = list_peers(&self.home).unwrap_or_default();
        let looking_for = match peers.as_slice() {
            [only] => Some(only.name.clone()),
            _ => None,
        };
        let _ = self.events.send(Event::Status(Status::Waiting {
            port,
            pairing,
            looking_for,
        }));
    }
}

impl ServiceObserver for ControllerObserver {
    fn waiting(&mut self, _name: &str, _key: PublicKey, port: u16, pairing: Option<Policy>) {
        self.waiting = Some((port, pairing.is_some()));
        self.send_waiting(port, pairing.is_some());
    }

    fn connecting(&mut self, address: &str, peer: Option<&str>) {
        let _ = self.events.send(Event::Status(Status::Connecting {
            address: address.to_owned(),
            peer: peer.map(str::to_owned),
        }));
    }

    fn reconnecting(&mut self, address: &str, peer: Option<&str>, wait: std::time::Duration) {
        let _ = self.events.send(Event::Status(Status::Reconnecting {
            address: address.to_owned(),
            peer: peer.map(str::to_owned),
            wait,
        }));
    }

    fn link(&mut self, peer: &str, key: PublicKey, link: crate::control::Link) {
        let _ = self.events.send(Event::Link { key, link });
        let now = trust::now();
        let fresh = self
            .stats
            .get(&key)
            .is_some_and(|kept| kept.stats == link.stats || kept.updated == now);
        self.stats.insert(
            key,
            crate::latency::LinkStats {
                peer: peer.to_owned(),
                updated: now,
                stats: link.stats,
            },
        );
        // at most once a second a link
        if !fresh {
            self.save_stats();
        }
    }

    fn connected(&mut self, peer: &str, key: PublicKey, _side: Side) {
        match self.linked.iter_mut().find(|(_, linked, _)| *linked == key) {
            Some((_, _, links)) => *links += 1,
            None => self.linked.push((peer.to_owned(), key, 1)),
        }
        self.send_connected();
    }

    fn arranged(&mut self, layout: &crate::share::Layout) {
        let _ = self.events.send(Event::Arranged(layout.clone()));
    }

    fn disconnected(&mut self, _peer: &str, key: PublicKey) {
        if let Some(index) = self.linked.iter().position(|(_, linked, _)| *linked == key) {
            self.linked[index].2 -= 1;
            if self.linked[index].2 == 0 {
                self.linked.remove(index);
                if self.stats.remove(&key).is_some() {
                    self.save_stats();
                }
            }
        }
        if !self.linked.is_empty() {
            self.send_connected();
        } else if let Some((port, pairing)) = self.waiting {
            self.send_waiting(port, pairing);
        }
    }

    fn pairing_opened(&mut self) {
        self.adding = true;
        let _ = self.events.send(Event::Adding(true));
    }

    fn pairing_closed(&mut self) {
        if std::mem::take(&mut self.adding) {
            let _ = self.events.send(Event::Adding(false));
        }
        if let Some((port, _)) = self.waiting {
            self.waiting = Some((port, false));
            self.send_waiting(port, false);
        }
    }

    fn paired(&mut self, peer: &str, key: PublicKey, _policy: Policy) {
        let _ = self.events.send(Event::Paired {
            peer: peer.to_owned(),
            key,
        });
        if let Ok(peers) = list_peers(&self.home) {
            let _ = self.events.send(Event::Peers(peers));
        }
    }

    fn connection_failed(&mut self, address: &str, error: &anyhow::Error) {
        tracing::warn!(address, error = ?error, "incoming connection ended");
        if let Some(copy) = version_copy(&self.home, error) {
            // automatic mode retries every few seconds; say it once
            if self.told_version.as_ref() != Some(&copy) {
                self.told_version = Some(copy.clone());
                let (title, detail) = copy;
                let _ = self.events.send(Event::Notice { title, detail });
            }
            return;
        }
        // anything else is retried, and the status already says so; an
        // alert here would come back on every retry
        if error.chain().any(|cause| cause.is::<crate::pairing::CodeMismatch>()) {
            let _ = self.events.send(Event::CodeMismatch);
        }
    }
}

fn list_peers(home: &Path) -> Result<Vec<Peer>> {
    PeerStore::open(home)?.list(trust::now())
}

/// Forgets the peer `selector` names here and, through a signed
/// revocation, across the group.
fn forget_peer(home: &Path, selector: &str) -> Result<Vec<Peer>> {
    let store = PeerStore::open(home)?;
    let now = trust::now();
    let me = Identity::load_or_create(&home.join("identity"))?.public_key();
    let signer = crate::introduce::Signer::load_or_create(&home.join("device-identity"))?;
    let forgotten = store.forget_revoking(&[selector.to_owned()], &signer, me, now)?;
    if forgotten.removed == 0 {
        bail!("no paired peer matches {selector:?}");
    }
    store.list(now)
}

fn set_trust(home: &Path, selector: &str, policy: Policy) -> Result<(String, Vec<Peer>)> {
    let store = PeerStore::open(home)?;
    let (matched, still) = store.set_policy(selector, policy, trust::now())?;
    if matched == 0 {
        bail!("no paired peer matches {selector:?}");
    }
    let peer = still
        .first()
        .map(|peer| peer.name.clone())
        .unwrap_or_else(|| selector.to_owned());
    Ok((peer, store.list(trust::now())?))
}

/// What to tell a person when the peer runs another protocol version: a
/// title, and which versions run where and which system to update.
fn version_copy(home: &Path, error: &anyhow::Error) -> Option<(String, String)> {
    let mismatch = error
        .chain()
        .find_map(|cause| match cause.downcast_ref::<SessionError>() {
            Some(SessionError::VersionMismatch(mismatch)) => Some(mismatch),
            _ => None,
        })?;
    let name = PeerStore::open(home)
        .and_then(|peers| peers.trusted(&mismatch.peer_key, trust::now()))
        .ok()
        .flatten()
        .map(|peer| peer.name);
    Some(mismatch_copy(mismatch, name.as_deref()))
}

fn mismatch_copy(mismatch: &Mismatch, name: Option<&str>) -> (String, String) {
    let peer = name.unwrap_or("The peer");
    let title = format!("{peer} runs a different version of Daisy");
    let update = if mismatch.update_here() {
        "this system".to_owned()
    } else {
        name.unwrap_or("the peer").to_owned()
    };
    let detail = format!(
        "{peer} runs Daisy {} (protocol {}). This system runs Daisy {} (protocol {}). Update Daisy on {update} to connect.",
        mismatch.peer.app, mismatch.peer.protocol, mismatch.local.app, mismatch.local.protocol
    );
    (title, detail)
}

fn recovery_for(error: &anyhow::Error) -> String {
    let message = format!("{error:#}");
    if message.contains("different system answered") {
        "Check the address. To use that system, pair with it on purpose.".to_owned()
    } else if message.contains("not paired") || message.contains("trust") {
        "Pair the systems again.".to_owned()
    } else if message.contains("refused the input tap") {
        "Quit and reopen Daisy. If it still fails, choose Reset Permissions in the Daisy menu.".to_owned()
    } else if message.contains("Permission") || message.contains("permission") {
        "Open Daisy and grant the missing macOS permission.".to_owned()
    } else if message.contains("connecting") {
        "Check that the other system is awake, waiting, and on the same network.".to_owned()
    } else {
        "Review the setup and try again.".to_owned()
    }
}

fn send_problem(events: &Sender<Event>, summary: &str, recovery: String) -> Result<()> {
    events
        .send(Event::Status(Status::Problem {
            summary: summary.to_owned(),
            recovery,
        }))
        .map_err(|_| anyhow::anyhow!("Daisy's interface closed"))
}

pub fn default_home() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join("Library/Application Support/daisy"))
}

pub fn computer_name() -> String {
    std::process::Command::new("/usr/sbin/scutil")
        .args(["--get", "ComputerName"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "System".to_owned())
}

fn load_settings(home: &Path) -> Result<AppSettings> {
    let path = home.join(SETTINGS_FILE);
    match fs::read_to_string(&path) {
        Ok(text) => toml::from_str(&text).with_context(|| format!("reading {}", path.display())),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(AppSettings::default()),
        Err(error) => Err(error).with_context(|| format!("reading {}", path.display())),
    }
}

fn save_settings(home: &Path, settings: &AppSettings) -> Result<()> {
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(home)
        .with_context(|| format!("creating {}", home.display()))?;
    let path = home.join(SETTINGS_FILE);
    let temporary = path.with_extension("tmp");
    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary)
        .with_context(|| format!("creating {}", temporary.display()))?;
    file.set_permissions(fs::Permissions::from_mode(0o600))?;
    file.write_all(toml::to_string_pretty(settings)?.as_bytes())?;
    file.sync_all()?;
    fs::rename(&temporary, &path).with_context(|| format!("saving {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_addition_keeps_the_running_group() {
        assert_eq!(address_action(true), AddressAction::Join);
        assert_eq!(address_action(false), AddressAction::Start);
    }

    #[test]
    fn saved_address_seeds_a_discovering_group() {
        let key = Identity::generate().unwrap().public_key();
        let settings = SessionSettings {
            connection: Connection::Connect {
                address: "192.168.1.20".to_owned(),
                peer: Some(key.to_hex()),
            },
            side: Side::Left,
            trust: Policy::Days(30),
        };
        let (group, address) = group_start(settings.clone());
        assert_eq!(group.connection, Connection::Automatic);
        assert_eq!(address, Some(("192.168.1.20".to_owned(), Some(key))));
        assert_eq!(group.side, settings.side);
        assert_eq!(group.trust, settings.trust);
        assert_eq!(group_start(SessionSettings::default()).1, None);
    }

    #[test]
    fn a_member_stays_connected_until_its_last_link_ends() {
        let directory = tempfile::tempdir().unwrap();
        let (events, received) = mpsc::channel();
        let mut observer = ControllerObserver {
            told_version: None,
            events,
            home: directory.path().to_owned(),
            waiting: Some((service::DEFAULT_PORT, false)),
            stats: std::collections::BTreeMap::new(),
            linked: Vec::new(),
            adding: false,
        };
        let studio = Identity::generate().unwrap().public_key();
        let desk = Identity::generate().unwrap().public_key();
        observer.connected("Studio", studio, Side::Right);
        observer.connected("Desk", desk, Side::Left);
        // a second connection from the studio overlaps the first, which then ends
        observer.connected("Studio", studio, Side::Right);
        observer.disconnected("Studio", studio);
        let last = std::iter::from_fn(|| received.try_recv().ok()).last();
        assert!(matches!(&last, Some(Event::Status(Status::Connected { peers })) if peers.len() == 2));
        observer.disconnected("Studio", studio);
        // two systems can share a name; each is still its own member
        let twin = Identity::generate().unwrap().public_key();
        observer.connected("Desk", twin, Side::Left);
        observer.disconnected("Desk", twin);
        let last = std::iter::from_fn(|| received.try_recv().ok()).last();
        assert!(
            matches!(&last, Some(Event::Status(Status::Connected { peers })) if peers == &[("Desk".to_owned(), desk)])
        );
        observer.disconnected("Desk", desk);
        let last = std::iter::from_fn(|| received.try_recv().ok()).last();
        assert!(matches!(last, Some(Event::Status(Status::Waiting { .. }))));
    }

    #[test]
    fn settings_round_trip_and_default_to_simple_path() {
        let directory = tempfile::tempdir().unwrap();
        let defaults = load_settings(directory.path()).unwrap();
        assert_eq!(defaults, AppSettings::default());
        assert_eq!(defaults.last_session.side, Side::Right);
        assert_eq!(defaults.last_session.trust, Policy::IDLE);

        let settings = AppSettings {
            last_session: SessionSettings {
                connection: Connection::Connect {
                    address: "studio.local".to_owned(),
                    peer: Some("00".repeat(32)),
                },
                side: Side::Right,
                trust: Policy::Days(30),
            },
            share_clipboard: false,
            always_discoverable: true,
            sharing: true,
        };
        save_settings(directory.path(), &settings).unwrap();
        assert_eq!(load_settings(directory.path()).unwrap(), settings);
        assert_eq!(
            fs::metadata(directory.path().join(SETTINGS_FILE))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }

    #[test]
    fn a_version_mismatch_says_which_system_to_update() {
        use crate::session::Version;
        let mismatch = Mismatch {
            peer_key: Identity::generate().unwrap().public_key(),
            peer: Version {
                protocol: 3,
                app: "0.1.3".to_owned(),
            },
            local: Version {
                protocol: 2,
                app: "0.1.2".to_owned(),
            },
        };
        let (title, detail) = mismatch_copy(&mismatch, Some("Studio"));
        assert_eq!(title, "Studio runs a different version of Daisy");
        assert_eq!(
            detail,
            "Studio runs Daisy 0.1.3 (protocol 3). This system runs Daisy 0.1.2 (protocol 2). Update Daisy on this system to connect."
        );
        let newer_here = Mismatch {
            peer: mismatch.local.clone(),
            local: mismatch.peer.clone(),
            ..mismatch
        };
        let (title, detail) = mismatch_copy(&newer_here, None);
        assert_eq!(title, "The peer runs a different version of Daisy");
        assert!(detail.ends_with("Update Daisy on the peer to connect."));

        let wrapped = anyhow::Error::from(SessionError::VersionMismatch(Box::new(newer_here))).context("connecting");
        let home = tempfile::tempdir().unwrap();
        assert!(version_copy(home.path(), &wrapped).is_some());
        assert!(version_copy(home.path(), &anyhow::anyhow!("connecting failed")).is_none());
    }

    #[test]
    fn recovery_copy_names_an_action() {
        let unpaired = anyhow::anyhow!("other system is not paired");
        assert!(recovery_for(&unpaired).starts_with("Pair the systems again."));
        let network = anyhow::anyhow!("connecting to studio.local failed");
        assert!(recovery_for(&network).starts_with("Check that the other system"));

        let changed = anyhow::Error::from(crate::reconnect::KeyChanged {
            address: "studio.local".into(),
        });
        assert!(recovery_for(&changed).starts_with("Check the address"));

        let tap = anyhow::anyhow!("macOS refused the input tap: Daisy needs Accessibility and Input Monitoring");
        assert!(recovery_for(&tap).starts_with("Quit and reopen Daisy"));
    }

    #[tokio::test]
    async fn closing_pairing_prompt_cancels_pairing() {
        let (events, received) = mpsc::channel();
        let mut prompt = ControllerPrompt { events };
        let answer = prompt.ask_code("Studio");
        let Event::AskPairingCode { peer, reply } = received.recv().unwrap() else {
            panic!("expected a pairing-code request");
        };
        assert_eq!(peer, "Studio");
        drop(reply);

        assert!(answer.await.is_err());
    }

    #[test]
    fn listener_reports_when_pairing_closes_and_after_disconnect() {
        let directory = tempfile::tempdir().unwrap();
        let (events, received) = mpsc::channel();
        let key = Identity::load_or_create(&directory.path().join("identity"))
            .unwrap()
            .public_key();
        let mut observer = ControllerObserver {
            told_version: None,
            events,
            home: directory.path().to_owned(),
            waiting: None,
            stats: std::collections::BTreeMap::new(),
            linked: Vec::new(),
            adding: false,
        };
        observer.waiting("Studio", key, service::DEFAULT_PORT, Some(Policy::IDLE));
        assert!(matches!(
            received.recv().unwrap(),
            Event::Status(Status::Waiting { pairing: true, .. })
        ));

        observer.pairing_closed();
        assert!(matches!(
            received.recv().unwrap(),
            Event::Status(Status::Waiting { pairing: false, .. })
        ));

        observer.disconnected("Desk", key);
        assert!(matches!(
            received.recv().unwrap(),
            Event::Status(Status::Waiting { pairing: false, .. })
        ));
    }

    #[test]
    fn adding_a_system_to_a_running_group_is_reported_open_then_closed() {
        let directory = tempfile::tempdir().unwrap();
        let (events, received) = mpsc::channel();
        let mut observer = ControllerObserver {
            told_version: None,
            events,
            home: directory.path().to_owned(),
            waiting: None,
            stats: std::collections::BTreeMap::new(),
            linked: Vec::new(),
            adding: false,
        };
        let next = || received.recv_timeout(std::time::Duration::from_secs(1)).unwrap();
        observer.pairing_opened();
        assert!(matches!(next(), Event::Adding(true)));
        observer.pairing_closed();
        assert!(matches!(next(), Event::Adding(false)));
        observer.pairing_closed();
        assert!(received.try_recv().is_err());
    }

    #[test]
    fn a_configuration_profile_overrides_always_discoverable() {
        assert_eq!(listening(true, true), Listening::Always);
        assert_eq!(listening(true, false), Listening::Closed);
        assert_eq!(listening(false, true), Listening::Closed);
    }

    #[test]
    fn a_dropped_peer_raises_no_alert_however_often_it_is_retried() {
        let directory = tempfile::tempdir().unwrap();
        let (events, received) = mpsc::channel();
        let mut observer = ControllerObserver {
            told_version: None,
            events,
            home: directory.path().to_owned(),
            waiting: None,
            stats: std::collections::BTreeMap::new(),
            linked: Vec::new(),
            adding: false,
        };
        for _ in 0..3 {
            observer.connection_failed("192.168.1.20:24850", &anyhow::anyhow!("connection refused"));
        }
        assert!(received.try_recv().is_err());
        observer.connection_failed("192.168.1.20:24850", &crate::pairing::CodeMismatch.into());
        assert!(matches!(received.try_recv(), Ok(Event::CodeMismatch)));
    }
}
