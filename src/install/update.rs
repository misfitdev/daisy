//! Staging and restart decisions. The platform executes process and bundle work.

use std::fs::{self, DirBuilder, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Build {
    pub version: String,
    pub protocol: u16,
}

impl Build {
    pub fn this_system() -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION").into(),
            protocol: crate::session::PROTOCOL,
        }
    }

    pub fn accepts(&self, next: &Self) -> bool {
        self.protocol == next.protocol
            && semver::Version::parse(&self.version)
                .ok()
                .zip(semver::Version::parse(&next.version).ok())
                .is_some_and(|(old, new)| new > old && new.pre.is_empty())
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Plan {
    pub target: PathBuf,
    pub home: PathBuf,
    pub previous: Build,
    pub next: Build,
    pub token: String,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ready {
    pub token: String,
    pub pid: u32,
}

/// Exactly one update can own a destination. A failed staging attempt removes
/// only the directory it created, never another transaction or the current app.
pub struct Staged {
    pub root: PathBuf,
    pub plan: Plan,
    handed_off: bool,
}

pub trait Platform {
    fn copy(&mut self, from: &Path, to: &Path) -> Result<()>;
    fn verify(&mut self, bundle: &Path, build: &Build) -> Result<()>;
    /// Returns only after all running copies at this path have exited.
    fn stop(&mut self, bundle: &Path, force: bool) -> Result<()>;
    fn listener_available(&mut self) -> Result<()>;
    /// Atomically exchanges two whole bundles; the installed path stays valid.
    fn exchange(&mut self, a: &Path, b: &Path) -> Result<()>;
    fn launch(&mut self, bundle: &Path, home: &Path, transaction: Option<&Path>) -> Result<()>;
    fn await_ready(&mut self, bundle: &Path, root: &Path, token: &str) -> Result<()>;
}

impl Staged {
    pub fn prepare(
        source: &Path,
        target: &Path,
        home: &Path,
        previous: Build,
        next: Build,
        platform: &mut impl Platform,
    ) -> Result<Self> {
        ensure!(
            previous.accepts(&next),
            "this update is not a newer compatible stable release"
        );
        let target = target.canonicalize().context("locating the installed app")?;
        ensure!(
            target.file_name().is_some_and(|name| name == "Daisy.app"),
            "expected the installed Daisy.app"
        );
        let root = target
            .parent()
            .context("the installed app has no folder")?
            .join(".Daisy.app.update");
        let token = crate::identity::Identity::generate()?.public_key().to_hex();
        let home = std::path::absolute(home)?;
        DirBuilder::new()
            .mode(0o700)
            .create(&root)
            .context("another update is pending, or the app folder is not writable")?;
        let staged = Self {
            root,
            plan: Plan {
                target,
                home,
                previous,
                next,
                token,
            },
            handed_off: false,
        };
        // Copy before verifying; verification covers the exact immutable staged
        // paths the helper will use, rather than a download that can change.
        platform.copy(source, &staged.candidate())?;
        platform.verify(&staged.candidate(), &staged.plan.next)?;
        platform.copy(&staged.plan.target, &staged.previous())?;
        platform.verify(&staged.previous(), &staged.plan.previous)?;
        write_new(
            &staged.root.join("plan.toml"),
            toml::to_string(&staged.plan)?.as_bytes(),
        )?;
        Ok(staged)
    }

    pub fn candidate(&self) -> PathBuf {
        self.root.join("candidate.app")
    }
    pub fn previous(&self) -> PathBuf {
        self.root.join("previous.app")
    }

    /// The retained old binary runs the helper, so replacement failure cannot
    /// remove the code responsible for restoring it.
    pub fn helper(&self) -> PathBuf {
        self.previous().join("Contents/MacOS/daisy")
    }

    pub fn hand_off(&mut self) {
        self.handed_off = true;
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        if !self.handed_off {
            let _ = fs::remove_dir_all(&self.root);
        }
    }
}

pub fn read_plan(root: &Path) -> Result<Plan> {
    let metadata = fs::symlink_metadata(root)?;
    ensure!(
        metadata.is_dir() && metadata.permissions().mode() & 0o077 == 0,
        "update directory is not private"
    );
    let plan: Plan = toml::from_str(&fs::read_to_string(root.join("plan.toml"))?)?;
    ensure!(
        plan.target.is_absolute() && plan.home.is_absolute(),
        "update paths must be absolute"
    );
    ensure!(
        plan.target
            .parent()
            .is_some_and(|parent| parent.join(".Daisy.app.update") == root),
        "update directory does not belong to this app"
    );
    ensure!(plan.previous.accepts(&plan.next), "update compatibility changed");
    Ok(plan)
}

/// The old process exits before the exchange or launch. On a launch/startup
/// failure the candidate must exit before the old bundle is restored.
pub fn restart(root: &Path, platform: &mut impl Platform) -> Result<()> {
    let plan = read_plan(root)?;
    let candidate = root.join("candidate.app");
    let previous = root.join("previous.app");
    if platform.verify(&plan.target, &plan.previous).is_err() {
        // A helper may have exited after the atomic exchange. The installed
        // path still holds a complete bundle; recover conservatively to the
        // retained old version rather than launch another candidate process.
        platform.verify(&plan.target, &plan.next)?;
        platform.verify(&candidate, &plan.previous)?;
        platform.verify(&previous, &plan.previous)?;
        platform.stop(&plan.target, true)?;
        platform.listener_available()?;
        platform.exchange(&candidate, &plan.target)?;
        platform.launch(&plan.target, &plan.home, None)?;
        fs::remove_dir_all(root)?;
        bail!("an interrupted update was recovered; the previous app was restored");
    }
    let before_exchange = (|| -> Result<()> {
        platform.verify(&candidate, &plan.next)?;
        platform.verify(&previous, &plan.previous)?;
        platform.stop(&plan.target, false)?;
        if let Err(error) = platform.listener_available() {
            let _ = platform.launch(&plan.target, &plan.home, None);
            return Err(error.context("the network listener is still in use; the update was not installed"));
        }
        if let Err(error) = platform.exchange(&candidate, &plan.target) {
            let _ = platform.launch(&plan.target, &plan.home, None);
            return Err(error.context("the app could not be replaced; the previous app was retained"));
        }
        Ok(())
    })();
    if let Err(error) = before_exchange {
        if let Err(cleanup) = fs::remove_dir_all(root) {
            return Err(error.context(format!(
                "the old app was retained but staging cleanup failed: {cleanup:#}"
            )));
        }
        return Err(error);
    }
    let started = platform
        .launch(&plan.target, &plan.home, Some(root))
        .and_then(|()| platform.await_ready(&plan.target, root, &plan.token));
    if let Err(error) = started {
        // Never overwrite a running candidate or start two listeners. If it
        // cannot be stopped, keep both complete bundles for recovery.
        platform
            .stop(&plan.target, true)
            .context("the replacement could not be stopped; the previous app is retained")?;
        platform
            .listener_available()
            .context("the replacement has not released the listener")?;
        platform
            .exchange(&candidate, &plan.target)
            .context("rollback could not restore the previous app; its backup is retained")?;
        platform
            .launch(&plan.target, &plan.home, None)
            .context("the previous app was restored but could not be reopened")?;
        // A restored transaction can be removed; the original is installed.
        fs::remove_dir_all(root).context("the previous app was restored but staging cleanup failed")?;
        bail!("the replacement did not start; the previous app was restored: {error:#}");
    }
    // A health acknowledgment, not merely a successful `open`, commits it.
    write_new(&root.join("committed"), b"ready\n")?;
    fs::remove_dir_all(root).context("the update started successfully but its backup could not be removed")?;
    Ok(())
}

fn write_new(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = OpenOptions::new().create_new(true).write(true).mode(0o600).open(path)?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}

#[derive(Debug, Clone, Copy)]
pub enum StartupState {
    UiReady,
    IdleReady,
    SharingReady,
    Failed,
}

/// UI and controller readiness must both arrive; a bind or setup failure must
/// not acknowledge success and delete the backup.
#[derive(Default)]
pub struct StartupGate {
    ui: bool,
    backend: bool,
    failed: bool,
}

impl StartupGate {
    pub fn observe(&mut self, state: StartupState) -> bool {
        match state {
            StartupState::UiReady => self.ui = true,
            StartupState::IdleReady | StartupState::SharingReady => self.backend = true,
            StartupState::Failed => self.failed = true,
        }
        self.ui && self.backend && !self.failed
    }
}

pub fn acknowledge(root: &Path, target: &Path, home: &Path) -> Result<()> {
    let plan = read_plan(root)?;
    ensure!(
        plan.next == Build::this_system(),
        "this process is not the replacement build"
    );
    ensure!(
        target.canonicalize()? == plan.target && home == plan.home,
        "this process is not the replacement installation"
    );
    write_new(
        &root.join("ready.toml"),
        toml::to_string(&Ready {
            token: plan.token,
            pid: std::process::id(),
        })?
        .as_bytes(),
    )
}
