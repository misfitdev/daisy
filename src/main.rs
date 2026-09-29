use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use tracing_subscriber::EnvFilter;

use daisy::identity::{Identity, PublicKey};
use daisy::input::Side;
use daisy::launcher;
use daisy::macos;
use daisy::pairing::TerminalPrompt;
use daisy::peers::{Peer, PeerStore};
use daisy::permissions::{self, Access};
use daisy::service::{self, ServiceObserver};
use daisy::trust::{self, Policy, Timestamp};

#[derive(Parser)]
#[command(version, about = "Share one keyboard, mouse and trackpad swipes between Macs")]
struct Cli {
    /// Directory holding this Mac's key and its paired Macs
    #[arg(long, env = "DAISY_HOME", global = true)]
    home: Option<PathBuf>,

    /// Name shown to the other Mac [default: this Mac's name]
    #[arg(long, global = true)]
    name: Option<String>,

    /// Open the menu-bar app when no command is given
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Show this Mac's name and key fingerprint
    Id,
    /// Wait for another Mac to connect
    Listen {
        /// Address to listen on
        #[arg(long, default_value = "0.0.0.0")]
        bind: String,
        /// TCP port to listen on
        #[arg(long, default_value_t = service::DEFAULT_PORT)]
        port: u16,
        /// Allow pairing with a Mac this one does not know yet
        #[arg(long)]
        pair: bool,
        /// How long to trust a Mac paired now
        #[arg(long, default_value = "idle", value_name = "POLICY")]
        trust: Policy,
        /// Share this Mac's keyboard and mouse; the other Mac sits on this side
        #[arg(long, value_name = "SIDE")]
        drive: Option<Side>,
        /// Do not share the clipboard when control crosses
        #[arg(long)]
        no_clipboard: bool,
        /// Do not advertise this Mac with Bonjour while waiting
        #[arg(long)]
        no_discovery: bool,
    },
    /// Connect to another Mac
    Connect {
        /// Host name or address, optionally followed by :port
        address: String,
        /// Allow pairing with a Mac this one does not know yet
        #[arg(long)]
        pair: bool,
        /// How long to trust a Mac paired now
        #[arg(long, default_value = "idle", value_name = "POLICY")]
        trust: Policy,
        /// Share this Mac's keyboard and mouse; the other Mac sits on this side
        #[arg(long, value_name = "SIDE")]
        drive: Option<Side>,
        /// Do not share the clipboard when control crosses
        #[arg(long)]
        no_clipboard: bool,
    },
    /// List paired Macs and how long each stays trusted
    Peers,
    /// Change how long a paired Mac stays trusted
    Trust {
        /// Name or fingerprint, shown by `peers`
        peer: String,
        /// idle, <days>d counted from pairing, once, or forever
        policy: Policy,
    },
    /// Show whether macOS lets the app read and send input
    Permissions {
        /// Show system prompts for anything not yet granted
        #[arg(long)]
        request: bool,
    },
    /// Stop trusting paired Macs, ending any session with them at once
    Forget {
        /// Names or fingerprints, shown by `peers`
        #[arg(required_unless_present = "all", conflicts_with = "all")]
        peers: Vec<String>,
        /// Forget every paired Mac
        #[arg(long)]
        all: bool,
    },
    /// Give this Mac a new key; every paired Mac must pair again
    RotateKey,
}

fn main() -> Result<()> {
    if let Some(code) = launcher::relaunch_as_app()? {
        std::process::exit(code);
    }

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    macos::restore_pointer();
    let home = cli.home.map_or_else(default_home, Ok)?;
    let name = cli.name.unwrap_or_else(computer_name);

    match cli.command {
        None => daisy::app::run(home, name),
        Some(command) => tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .context("starting the async runtime")?
            .block_on(run_cli(command, home, name)),
    }
}

async fn run_cli(command: Command, home: PathBuf, name: String) -> Result<()> {
    let identity = Identity::load_or_create(&home.join("identity"))?;
    let peers = PeerStore::open(&home)?;

    tokio::select! {
        result = execute(command, &home, &identity, &peers, &name) => result,
        () = launcher::launcher_gone() => {
            tracing::info!("the terminal that launched Daisy exited; stopping");
            Ok(())
        }
    }
}

async fn execute(command: Command, home: &Path, identity: &Identity, peers: &PeerStore, name: &str) -> Result<()> {
    match command {
        Command::Id => {
            println!("{name}\n{}", identity.public_key());
            Ok(())
        }
        Command::Listen {
            bind,
            port,
            pair,
            trust,
            drive,
            no_clipboard,
            no_discovery,
        } => {
            let mut prompt = TerminalPrompt;
            let mut observer = TerminalObserver;
            let (_share_clipboard, clipboard) = tokio::sync::watch::channel(!no_clipboard);
            let (_discoverable, discoverable) = tokio::sync::watch::channel(!no_discovery);
            service::listen(
                service::SessionConfig {
                    identity,
                    peers,
                    name,
                    pairing: pair.then_some(trust),
                    drive,
                    clipboard: &clipboard,
                    discoverable: &discoverable,
                },
                &bind,
                port,
                &mut prompt,
                &mut observer,
            )
            .await
        }
        Command::Connect {
            address,
            pair,
            trust,
            drive,
            no_clipboard,
        } => {
            let mut prompt = TerminalPrompt;
            let mut observer = TerminalObserver;
            let (_share_clipboard, clipboard) = tokio::sync::watch::channel(!no_clipboard);
            let (_discoverable, discoverable) = tokio::sync::watch::channel(false);
            service::connect(
                service::SessionConfig {
                    identity,
                    peers,
                    name,
                    pairing: pair.then_some(trust),
                    drive,
                    clipboard: &clipboard,
                    discoverable: &discoverable,
                },
                &address,
                None,
                &mut prompt,
                &mut observer,
            )
            .await
        }
        Command::Peers => {
            let now = trust::now();
            let list = peers.list(now)?;
            if list.is_empty() {
                println!("No paired Macs.");
            } else {
                for peer in list {
                    println!(
                        "{}\t{}\t{}\t{}",
                        peer.name,
                        peer.key,
                        peer.policy.describe(),
                        status(&peer, now)
                    );
                }
            }
            Ok(())
        }
        Command::Trust { peer, policy } => {
            let now = trust::now();
            let (matched, still) = peers.set_policy(&peer, policy, now)?;
            if matched == 0 {
                bail!("no paired Mac matches {peer:?}");
            }
            for peer in &still {
                println!(
                    "{} is now trusted {}: {}",
                    peer.name,
                    policy.describe(),
                    status(peer, now)
                );
            }
            if still.len() < matched {
                println!(
                    "{} Mac(s) matching {peer:?} had already run out of trust under that policy and were forgotten.",
                    matched - still.len()
                );
            }
            Ok(())
        }
        Command::Permissions { request } => {
            show_permissions(request);
            Ok(())
        }
        Command::Forget { peers: selectors, all } => {
            let now = trust::now();
            let (removed, unmatched) = if all {
                (peers.forget_all(now)?, Vec::new())
            } else {
                let forgotten = peers.forget(&selectors, now)?;
                (forgotten.removed, forgotten.unmatched)
            };
            if removed > 0 {
                println!("Forgot {removed} Mac(s). Any matching session ends within a second.");
            } else if all {
                println!("No paired Macs to forget.");
            }
            if !unmatched.is_empty() {
                bail!("no paired Mac matches {}", unmatched.join(", "));
            }
            Ok(())
        }
        Command::RotateKey => {
            let rotated = Identity::rotate(&home.join("identity"))?;
            println!(
                "This Mac's key changed from {} to {}.\nEvery paired Mac must pair again. Restart Daisy if it is running.",
                identity.public_key(),
                rotated.public_key()
            );
            Ok(())
        }
    }
}

struct TerminalObserver;

impl ServiceObserver for TerminalObserver {
    fn waiting(&mut self, name: &str, key: PublicKey, port: u16, pairing: Option<Policy>) {
        println!("{name} ({key}) is waiting on port {port}.");
        if let Some(policy) = pairing {
            println!("Pairing is open; a new Mac will be trusted {}.", policy.describe());
        }
    }

    fn connecting(&mut self, address: &str) {
        println!("Connecting to {address}…");
    }

    fn reconnecting(&mut self, address: &str, wait: std::time::Duration) {
        println!(
            "The connection to {address} was lost; trying again in {} s.",
            wait.as_secs().max(1)
        );
    }

    fn paired(&mut self, peer: &str, key: PublicKey, policy: Policy) {
        println!("Paired {peer} ({key}), trusted {}.", policy.describe());
        match policy {
            Policy::Once => println!("The next connection will ask for a code again."),
            Policy::Forever => {
                println!("It stays trusted until you run `daisy forget`, however long it goes unused.")
            }
            Policy::Idle | Policy::Days(_) => {}
        }
    }

    fn connected(&mut self, peer: &str, _key: PublicKey, drive: Option<Side>) {
        match drive {
            Some(side) => {
                println!("Sharing this keyboard and mouse with {peer}: move the pointer off the {side} edge to use it.")
            }
            None => println!("{peer} can now control this Mac."),
        }
    }

    fn disconnected(&mut self, peer: &str) {
        println!("{peer} disconnected.");
    }

    fn connection_failed(&mut self, address: &str, error: &anyhow::Error) {
        eprintln!("Connection from {address} ended: {error:#}");
    }
}

fn status(peer: &Peer, now: Timestamp) -> String {
    match (peer.policy, peer.expires_at()) {
        (_, None) => "never expires".to_owned(),
        (Policy::Once, Some(_)) => "ends after its session".to_owned(),
        (Policy::Idle, Some(at)) => {
            format!("ends in {} without a connection", trust::span(at.saturating_sub(now)))
        }
        (_, Some(at)) => format!("ends in {}", trust::span(at.saturating_sub(now))),
    }
}

fn show_permissions(request: bool) {
    let (accessibility, input_monitoring) = if request {
        (
            permissions::request_accessibility(),
            permissions::request_input_monitoring(),
        )
    } else {
        (permissions::accessibility(), permissions::input_monitoring())
    };
    println!("Accessibility: {}", access_name(accessibility));
    println!("Input Monitoring: {}", access_name(input_monitoring));
    if accessibility != Access::Granted || input_monitoring != Access::Granted {
        println!("Open System Settings → Privacy & Security to grant Daisy.app access.");
    }
}

fn access_name(access: Access) -> &'static str {
    match access {
        Access::Granted => "granted",
        Access::Denied => "not granted",
        Access::Undetermined => "not requested",
    }
}

fn default_home() -> Result<PathBuf> {
    let home = std::env::var_os("HOME").context("HOME is not set")?;
    Ok(PathBuf::from(home).join("Library/Application Support/daisy"))
}

fn computer_name() -> String {
    std::process::Command::new("/usr/sbin/scutil")
        .args(["--get", "ComputerName"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|name| name.trim().to_owned())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "Mac".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_subcommand_is_the_menu_bar_app() {
        let cli = Cli::try_parse_from(["daisy"]).unwrap();
        assert!(cli.command.is_none());
    }
}
