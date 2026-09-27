//! Background controller for the native menu-bar application.
//!
//! AppKit stays on the main thread. This module owns the Tokio runtime and
//! communicates with it through typed channels, so no native callback waits on
//! network or disk work.

use std::fs::{self, DirBuilder, OpenOptions};
use std::io::{ErrorKind, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc as tokio_mpsc, oneshot, watch};

use crate::identity::{Identity, PublicKey};
use crate::input::Side;
use crate::pairing::{PairingCode, PairingPrompt};
use crate::peers::{Peer, PeerStore};
use crate::service::{self, ServiceObserver};
use crate::trust::{self, Policy};

const SETTINGS_FILE: &str = "settings.toml";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "snake_case")]
pub enum Connection {
    Listen {
        #[serde(default = "default_bind")]
        bind: String,
        #[serde(default = "default_port")]
        port: u16,
    },
    Connect {
        address: String,
    },
}

impl Default for Connection {
    fn default() -> Self {
        Self::Listen {
            bind: default_bind(),
            port: service::DEFAULT_PORT,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSettings {
    #[serde(flatten)]
    pub connection: Connection,
    /// `Some` means this system drives the peer through that edge.
    /// `None` means this system follows the peer.
    #[serde(default)]
    pub drive: Option<Side>,
    #[serde(default)]
    pub trust: Policy,
}

impl Default for SessionSettings {
    fn default() -> Self {
        Self {
            connection: Connection::default(),
            drive: Some(Side::Right),
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
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            last_session: SessionSettings::default(),
            share_clipboard: default_share_clipboard(),
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
        allow_pairing: bool,
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
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Idle,
    Starting,
    Waiting {
        port: u16,
        pairing: bool,
    },
    Connecting {
        address: String,
    },
    /// The connection was lost or could not be made; trying again in `wait`.
    Reconnecting {
        address: String,
        wait: std::time::Duration,
    },
    Connected {
        peer: String,
        key: PublicKey,
        drive: Option<Side>,
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
    TrustChanged {
        peer: String,
        policy: Policy,
    },
    Paired {
        peer: String,
        policy: Policy,
    },
    Notice {
        title: String,
        detail: String,
    },
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
    let settings = load_settings(&home).unwrap_or_else(|error| {
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
    let mut stored = settings.clone();
    let (share_clipboard, clipboard) = watch::channel(stored.share_clipboard);
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
                session = Some(tokio::spawn(async move {
                    run_session(
                        session_home,
                        session_name,
                        settings,
                        allow_pairing,
                        session_clipboard,
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
                Ok((peer, peers)) => {
                    let _ = events.send(Event::TrustChanged { peer, policy });
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
            Command::Shutdown => {
                if let Some(running) = session.take() {
                    running.abort();
                }
                break;
            }
        }
    }
}

async fn run_session(
    home: PathBuf,
    name: String,
    settings: SessionSettings,
    allow_pairing: bool,
    clipboard: watch::Receiver<bool>,
    events: Sender<Event>,
) {
    let result = async {
        let identity = Identity::load_or_create(&home.join("identity"))?;
        let peers = PeerStore::open(&home)?;
        let mut prompt = ControllerPrompt { events: events.clone() };
        let mut observer = ControllerObserver {
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
            drive: settings.drive,
            clipboard: &clipboard,
        };
        match settings.connection {
            Connection::Listen { bind, port } => service::listen(config, &bind, port, &mut prompt, &mut observer).await,
            Connection::Connect { address } => service::connect(config, &address, &mut prompt, &mut observer).await,
        }
    }
    .await;

    match result {
        Ok(()) => {
            let _ = events.send(Event::Status(Status::Idle));
        }
        Err(error) => {
            tracing::error!(error = ?error, "connection session stopped");
            let _ = send_problem(&events, "Connection stopped", recovery_for(&error));
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
    events: Sender<Event>,
    home: PathBuf,
    waiting: Option<(u16, bool)>,
}

impl ServiceObserver for ControllerObserver {
    fn waiting(&mut self, _name: &str, _key: PublicKey, port: u16, pairing: Option<Policy>) {
        self.waiting = Some((port, pairing.is_some()));
        let _ = self.events.send(Event::Status(Status::Waiting {
            port,
            pairing: pairing.is_some(),
        }));
    }

    fn connecting(&mut self, address: &str) {
        let _ = self.events.send(Event::Status(Status::Connecting {
            address: address.to_owned(),
        }));
    }

    fn reconnecting(&mut self, address: &str, wait: std::time::Duration) {
        let _ = self.events.send(Event::Status(Status::Reconnecting {
            address: address.to_owned(),
            wait,
        }));
    }

    fn connected(&mut self, peer: &str, key: PublicKey, drive: Option<Side>) {
        let _ = self.events.send(Event::Status(Status::Connected {
            peer: peer.to_owned(),
            key,
            drive,
        }));
    }

    fn disconnected(&mut self, _peer: &str) {
        if let Some((port, pairing)) = self.waiting {
            let _ = self.events.send(Event::Status(Status::Waiting { port, pairing }));
        }
    }

    fn pairing_closed(&mut self) {
        if let Some((port, _)) = self.waiting {
            self.waiting = Some((port, false));
            let _ = self
                .events
                .send(Event::Status(Status::Waiting { port, pairing: false }));
        }
    }

    fn paired(&mut self, peer: &str, _key: PublicKey, policy: Policy) {
        let _ = self.events.send(Event::Paired {
            peer: peer.to_owned(),
            policy,
        });
        if let Ok(peers) = list_peers(&self.home) {
            let _ = self.events.send(Event::Peers(peers));
        }
    }

    fn connection_failed(&mut self, address: &str, error: &anyhow::Error) {
        tracing::warn!(address, error = ?error, "incoming connection ended");
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

fn recovery_for(error: &anyhow::Error) -> String {
    let message = format!("{error:#}");
    if message.contains("different Mac answered") {
        "Check the address. To use that Mac, pair with it on purpose.".to_owned()
    } else if message.contains("not paired") || message.contains("trust") {
        "Pair the systems again.".to_owned()
    } else if message.contains("Permission") || message.contains("permission") {
        "Open Daisy and grant the missing macOS permission.".to_owned()
    } else if message.contains("not driving") || message.contains("set to drive") || message.contains("set as Host") {
        "Choose Host on exactly one system and Guest on the other.".to_owned()
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

fn default_bind() -> String {
    "0.0.0.0".to_owned()
}

const fn default_port() -> u16 {
    service::DEFAULT_PORT
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn settings_round_trip_and_default_to_simple_path() {
        let directory = tempfile::tempdir().unwrap();
        let defaults = load_settings(directory.path()).unwrap();
        assert_eq!(defaults, AppSettings::default());
        assert_eq!(defaults.last_session.drive, Some(Side::Right));
        assert_eq!(defaults.last_session.trust, Policy::Idle);

        let settings = AppSettings {
            last_session: SessionSettings {
                connection: Connection::Connect {
                    address: "studio.local".to_owned(),
                },
                drive: None,
                trust: Policy::Days(30),
            },
            share_clipboard: false,
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
    fn settings_saved_before_clipboard_sharing_load_with_it_on() {
        let directory = tempfile::tempdir().unwrap();
        fs::write(
            directory.path().join(SETTINGS_FILE),
            "[last_session]\nmode = \"connect\"\naddress = \"studio.local\"\n",
        )
        .unwrap();
        let loaded = load_settings(directory.path()).unwrap();
        assert!(loaded.share_clipboard);
        assert_eq!(
            loaded.last_session.connection,
            Connection::Connect {
                address: "studio.local".to_owned()
            }
        );
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

        let roles = anyhow::anyhow!("the other system is not driving");
        assert_eq!(
            recovery_for(&roles),
            "Choose Host on exactly one system and Guest on the other."
        );
    }

    #[tokio::test]
    async fn closing_pairing_prompt_cancels_pairing() {
        let (events, received) = mpsc::channel();
        let mut prompt = ControllerPrompt { events };
        let answer = prompt.ask_code("Studio Mac");
        let Event::AskPairingCode { peer, reply } = received.recv().unwrap() else {
            panic!("expected a pairing-code request");
        };
        assert_eq!(peer, "Studio Mac");
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
            events,
            home: directory.path().to_owned(),
            waiting: None,
        };
        observer.waiting("Studio", key, service::DEFAULT_PORT, Some(Policy::Idle));
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
