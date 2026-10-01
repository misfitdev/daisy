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

use crate::discovery;
use crate::identity::{Identity, PublicKey};
use crate::input::Side;
use crate::pairing::{PairingCode, PairingPrompt};
use crate::peers::{Peer, PeerStore};
use crate::service::{self, ServiceObserver};
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
    /// Advertise this system with Bonjour while it waits for a connection.
    #[serde(default = "default_discoverable")]
    pub discoverable: bool,
    /// Command-Q quits Daisy, rather than closing its windows.
    #[serde(default)]
    pub command_q_quits: bool,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            last_session: SessionSettings::default(),
            share_clipboard: default_share_clipboard(),
            discoverable: default_discoverable(),
            command_q_quits: false,
        }
    }
}

fn default_discoverable() -> bool {
    true
}

fn default_share_clipboard() -> bool {
    true
}

#[derive(Debug)]
pub enum Command {
    Start {
        settings: SessionSettings,
        allow_pairing: bool,
        /// The screen edge was chosen for this start, rather than left as saved.
        side_chosen: bool,
    },
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
    /// Turn Bonjour advertising on or off, including while waiting.
    SetDiscoverable(bool),
    /// Choose whether Command-Q quits Daisy or closes its windows.
    SetCommandQQuits(bool),
    /// Move the connected peer's screen to this side.
    Arrange(Side),
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
    Connected {
        peer: String,
        key: PublicKey,
        side: Side,
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
    },
    Status(Status),
    Peers(Vec<Peer>),
    ShowPairingCode {
        peer: String,
        code: PairingCode,
    },
    AskPairingCode {
        peer: String,
        reply: oneshot::Sender<String>,
    },
    Paired {
        peer: String,
        key: PublicKey,
        policy: Policy,
    },
    Notice {
        title: String,
        detail: String,
    },
    /// Peers, and systems open to pairing, found on the network.
    Nearby(Vec<Nearby>),
    /// Latency or who has control changed in the running session.
    Link(crate::control::Link),
}

/// A system found on the network, as the interface shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Nearby {
    /// The peer's name, or `None` for a system open to pairing.
    pub name: Option<String>,
    pub key: Option<PublicKey>,
    pub address: String,
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
    let (share_discoverable, discoverable) = watch::channel(stored.discoverable);
    let (arrange, arrangement) = watch::channel(None);
    tokio::spawn(browse_nearby(home.clone(), events.clone()));
    if events
        .send(Event::Ready {
            settings,
            peers,
            first_run,
        })
        .is_err()
    {
        return;
    }

    let mut session: Option<tokio::task::JoinHandle<()>> = None;
    while let Some(command) = commands.recv().await {
        match command {
            Command::Start {
                settings,
                allow_pairing,
                side_chosen,
            } => {
                if let Some(running) = session.take() {
                    running.abort();
                }
                stored.last_session = settings.clone();
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
                let session_clipboard = clipboard.clone();
                let session_discoverable = discoverable.clone();
                let session_arrangement = arrangement.clone();
                session = Some(tokio::spawn(async move {
                    let start = Start {
                        settings,
                        allow_pairing,
                        side_chosen,
                    };
                    run_session(
                        session_home,
                        session_name,
                        start,
                        session_clipboard,
                        session_discoverable,
                        session_arrangement,
                        session_events,
                    )
                    .await;
                }));
            }
            Command::Stop => {
                if let Some(running) = session.take() {
                    running.abort();
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
            Command::Arrange(side) => {
                arrange.send_replace(Some(side));
            }
            Command::SetCommandQQuits(on) => {
                stored.command_q_quits = on;
                if let Err(error) = save_settings(&home, &stored) {
                    tracing::error!(error = ?error, "Command-Q setting could not be saved");
                    let _ = send_problem(
                        &events,
                        "Command-Q setting could not be saved",
                        "Check that Daisy can write its data folder, then try again.".to_owned(),
                    );
                }
            }
            Command::SetDiscoverable(on) => {
                stored.discoverable = on;
                share_discoverable.send_replace(on);
                if let Err(error) = save_settings(&home, &stored) {
                    tracing::error!(error = ?error, "discovery setting could not be saved");
                    let _ = send_problem(
                        &events,
                        "Discovery setting could not be saved",
                        "Check that Daisy can write its data folder, then try again.".to_owned(),
                    );
                }
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

/// How often the Nearby list re-reads which peers are trusted, so a peer
/// forgotten, re-trusted, newly paired or expired is shown correctly even
/// when the network is quiet.
const NEARBY_TRUST_CHECK: std::time::Duration = std::time::Duration::from_secs(2);

/// The Nearby list for the systems heard on the network, given this system's
/// peers now. Only trusted peers are named; others appear only while
/// open to pairing.
fn nearby(found: Vec<discovery::Found>, peers: &[Peer]) -> Vec<Nearby> {
    let mut nearby: Vec<Nearby> = found
        .into_iter()
        .map(|heard| match heard.seen {
            discovery::Seen::Paired(key) => Nearby {
                name: peers.iter().find(|peer| peer.key == key).map(|peer| peer.name.clone()),
                key: Some(key),
                address: heard.address.to_string(),
            },
            discovery::Seen::Pairing => Nearby {
                name: None,
                key: None,
                address: heard.address.to_string(),
            },
        })
        .collect();
    nearby.sort_by(|a, b| (a.name.is_none(), &a.name, &a.address).cmp(&(b.name.is_none(), &b.name, &b.address)));
    nearby
}

/// Reports the systems found on the network whenever that list changes, from
/// either the network or this system's trust.
async fn browse_nearby(home: PathBuf, events: Sender<Event>) {
    let mut browser = match discovery::Browser::start() {
        Ok(browser) => browser,
        Err(error) => {
            tracing::warn!(error = format!("{error:#}"), "could not browse with Bonjour");
            return;
        }
    };
    let mut shown = None;
    let mut check = tokio::time::interval(NEARBY_TRUST_CHECK);
    loop {
        tokio::select! {
            more = browser.changed() => if !more { return },
            _ = check.tick() => {}
        }
        let peers = list_peers(&home).unwrap_or_default();
        let keys: Vec<PublicKey> = peers.iter().map(|peer| peer.key).collect();
        let list = nearby(browser.current(&keys), &peers);
        if shown.as_ref() == Some(&list) {
            continue;
        }
        if events.send(Event::Nearby(list.clone())).is_err() {
            return;
        }
        shown = Some(list);
    }
}

/// One press of Start, as `Command::Start` carries it.
struct Start {
    settings: SessionSettings,
    allow_pairing: bool,
    side_chosen: bool,
}

async fn run_session(
    home: PathBuf,
    name: String,
    start: Start,
    clipboard: watch::Receiver<bool>,
    discoverable: watch::Receiver<bool>,
    arrangement: watch::Receiver<Option<Side>>,
    events: Sender<Event>,
) {
    let Start {
        settings,
        allow_pairing,
        side_chosen,
    } = start;
    let result = async {
        let identity = Identity::load_or_create(&home.join("identity"))?;
        let peers = PeerStore::open(&home)?;
        let choose_side = AtomicBool::new(side_chosen);
        let mut prompt = ControllerPrompt { events: events.clone() };
        let mut observer = ControllerObserver {
            told_version: None,
            events: events.clone(),
            home: home.clone(),
            waiting: None,
        };
        let pairing = allow_pairing.then_some(settings.trust);
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
        };
        match settings.connection {
            Connection::Automatic => service::automatic(config, &mut prompt, &mut observer).await,
            Connection::Connect { address, peer } => {
                let peer = peer.as_deref().and_then(PublicKey::from_hex);
                service::connect(config, &address, peer, &mut prompt, &mut observer).await
            }
        }
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

    fn link(&mut self, _peer: &str, link: crate::control::Link) {
        let _ = self.events.send(Event::Link(link));
    }

    fn connected(&mut self, peer: &str, key: PublicKey, side: Side) {
        let _ = self.events.send(Event::Status(Status::Connected {
            peer: peer.to_owned(),
            key,
            side,
        }));
    }

    fn disconnected(&mut self, _peer: &str) {
        if let Some((port, pairing)) = self.waiting {
            self.send_waiting(port, pairing);
        }
    }

    fn pairing_closed(&mut self) {
        if let Some((port, _)) = self.waiting {
            self.waiting = Some((port, false));
            self.send_waiting(port, false);
        }
    }

    fn paired(&mut self, peer: &str, key: PublicKey, policy: Policy) {
        let _ = self.events.send(Event::Paired {
            peer: peer.to_owned(),
            key,
            policy,
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
        let _ = self.events.send(Event::Notice {
            title: "A peer could not connect".to_owned(),
            detail: format!(
                "Daisy is still waiting. If {address} is the other system, choose Pair a New Peer on both systems and try again."
            ),
        });
    }
}

fn list_peers(home: &Path) -> Result<Vec<Peer>> {
    PeerStore::open(home)?.list(trust::now())
}

fn forget_peer(home: &Path, selector: &str) -> Result<Vec<Peer>> {
    let store = PeerStore::open(home)?;
    let forgotten = store.forget(&[selector.to_owned()], trust::now())?;
    if forgotten.removed == 0 {
        bail!("no paired peer matches {selector:?}");
    }
    store.list(trust::now())
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
    fn nearby_names_only_trusted_peers() {
        let studio = Identity::generate().unwrap().public_key();
        let address: std::net::SocketAddr = "192.168.1.9:24850".parse().unwrap();
        let found = vec![discovery::Found {
            election: [0; 16],
            seen: discovery::Seen::Paired(studio),
            address,
        }];
        let peer = Peer {
            side: Side::Right,
            side_chosen: 0,
            name: "Studio".into(),
            key: studio,
            policy: Policy::IDLE,
            paired_at: 0,
            last_seen: 0,
        };
        let list = nearby(found.clone(), &[peer]);
        assert_eq!(list[0].name.as_deref(), Some("Studio"));
        assert_eq!(list[0].address, "192.168.1.9:24850");
        // a key that is no longer among the peers is never named
        assert_eq!(nearby(found, &[])[0].name, None);
    }
    use std::os::unix::fs::PermissionsExt;

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
            discoverable: false,
            command_q_quits: true,
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

        observer.disconnected("Desk");
        assert!(matches!(
            received.recv().unwrap(),
            Event::Status(Status::Waiting { pairing: false, .. })
        ));
    }
}
