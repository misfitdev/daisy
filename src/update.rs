//! Pure update settings, release compatibility channels, and status persistence.
use anyhow::{Context, Result};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Launch {
    pub home: PathBuf,
    pub name: String,
}

pub struct PendingRelaunch<T> {
    pending: Option<(String, T)>,
}

impl<T> Default for PendingRelaunch<T> {
    fn default() -> Self {
        Self { pending: None }
    }
}

impl<T> PendingRelaunch<T> {
    pub fn postpone(&mut self, version: String, continuation: T) {
        self.pending = Some((version, continuation));
    }

    pub fn clear(&mut self) {
        self.pending = None;
    }

    pub fn retry(&mut self, save: impl FnOnce(&str) -> Result<()>) -> Result<Option<T>> {
        if let Some((version, _)) = &self.pending {
            save(version)?;
        }
        Ok(self.pending.take().map(|(_, continuation)| continuation))
    }
}

#[derive(Serialize, Deserialize)]
struct Relaunch {
    executable: PathBuf,
    version: String,
    at: crate::trust::Timestamp,
    launch: Launch,
}

fn relaunch_path(directory: &Path, executable: &Path) -> PathBuf {
    use std::os::unix::ffi::OsStrExt;
    directory.join(format!(
        "update-relaunch-{}.toml",
        archive_sha256(executable.as_os_str().as_bytes())
    ))
}

/// Sparkle opens the replacement bundle without the original command-line arguments.
pub fn save_relaunch(
    directory: &Path,
    executable: &Path,
    version: &str,
    launch: &Launch,
    at: crate::trust::Timestamp,
) -> Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(directory)
        .with_context(|| format!("preparing update relaunch state in {}", directory.display()))?;
    let launch = Launch {
        home: std::path::absolute(&launch.home)?,
        name: launch.name.clone(),
    };
    let record = Relaunch {
        executable: executable.to_owned(),
        version: version.to_owned(),
        at,
        launch,
    };
    crate::state_file::write(
        &relaunch_path(directory, executable),
        toml::to_string(&record)?.as_bytes(),
    )
}

/// Consume a short-lived handoff only for this executable and release.
pub fn take_relaunch(
    directory: &Path,
    executable: &Path,
    version: &str,
    now: crate::trust::Timestamp,
) -> Result<Option<Launch>> {
    let path = relaunch_path(directory, executable);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("reading update relaunch state"),
    };
    // Only the process that removes the handoff may use it.
    match std::fs::remove_file(&path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("consuming update relaunch state"),
    }
    let record: Relaunch = toml::from_str(&text).context("decoding update relaunch state")?;
    Ok(
        (record.executable == executable && record.version == version && record.at <= now && now - record.at <= 300)
            .then_some(record.launch),
    )
}

pub fn restore_launch(
    home: Option<PathBuf>,
    name: Option<String>,
    relaunch: Option<Launch>,
) -> (Option<PathBuf>, Option<String>) {
    match (home, relaunch) {
        (Some(home), _) => (Some(home), name),
        (None, Some(launch)) => (Some(launch.home), name.or(Some(launch.name))),
        (None, None) => (None, name),
    }
}
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Policy {
    #[default]
    NotifyOnly,
    InstallAll,
    InstallMinorAndPatch,
}

impl Policy {
    /// Shown on the policy picker, in `ORDER`.
    pub fn label(self) -> &'static str {
        match self {
            Self::NotifyOnly => "Ask before installing",
            Self::InstallMinorAndPatch => "Auto-install routine updates",
            Self::InstallAll => "Auto-install every update",
        }
    }

    /// The gray line under the picker: what this policy actually does,
    /// in terms of the consequence a person cares about rather than
    /// semver tiers.
    pub fn caption(self) -> &'static str {
        match self {
            Self::NotifyOnly => "You'll be notified; nothing installs on its own.",
            Self::InstallMinorAndPatch => "Skips releases that could break compatibility with peers.",
            Self::InstallAll => "Includes releases that could require every connected peer to update too.",
        }
    }
}

/// The policy picker's items, in display order.
pub const ORDER: [Policy; 3] = [Policy::NotifyOnly, Policy::InstallMinorAndPatch, Policy::InstallAll];

/// Release compatibility metadata, bound to the final installation archive.
/// The feed producer checks the archive hash before signing update entries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub format: u32,
    pub version: String,
    pub protocol: u16,
    #[serde(default)]
    pub supported_protocols: Vec<u16>,
    pub archive_sha256: String,
}

impl Manifest {
    pub fn for_archive(archive: &[u8]) -> Self {
        Self {
            format: 2,
            version: env!("CARGO_PKG_VERSION").to_owned(),
            protocol: crate::session::PROTOCOL,
            supported_protocols: crate::session::supported_protocols(&crate::session::Version::this_system()),
            archive_sha256: archive_sha256(archive),
        }
    }
}

fn archive_sha256(archive: &[u8]) -> String {
    Sha256::digest(archive)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastCheck {
    pub at: crate::trust::Timestamp,
    /// The newer release found then, if any.
    pub available: Option<String>,
}

const LAST_CHECK_FILE: &str = "last-update-check.toml";

/// The record of the last successful check in `home`, if there is one.
pub fn load_last_check(home: &std::path::Path) -> Option<LastCheck> {
    let text = std::fs::read_to_string(home.join(LAST_CHECK_FILE)).ok()?;
    toml::from_str(&text).ok()
}

pub fn save_last_check(home: &std::path::Path, last: &LastCheck) -> Result<()> {
    crate::state_file::write(&home.join(LAST_CHECK_FILE), toml::to_string(last)?.as_bytes())
}

/// The gray status line in the Updates panel. `ago` renders a past time
/// relative to now, such as "5 minutes ago" or "yesterday".
pub fn status_line(last: Option<&LastCheck>, failed: bool, ago: impl Fn(crate::trust::Timestamp) -> String) -> String {
    match (last, failed) {
        (None, false) => "Not checked yet.".to_owned(),
        (None, true) => "Could not check for updates.".to_owned(),
        (Some(last), true) => format!("Could not check for updates. Last checked {}.", ago(last.at)),
        (Some(last), false) => match &last.available {
            None => format!("Daisy is up to date. Checked {}.", ago(last.at)),
            Some(version) => format!("Daisy {version} is available. Checked {}.", ago(last.at)),
        },
    }
}

/// The animated "Checking" line: no dots, then one, two and three, repeating.
pub fn checking_line(step: usize) -> String {
    format!("Checking{}", ".".repeat(step % 4))
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Build {
    pub version: String,
    pub protocol: u16,
    #[serde(default)]
    pub supported_protocols: Vec<u16>,
}

impl Build {
    pub fn this_system() -> Self {
        Self {
            version: env!("CARGO_PKG_VERSION").into(),
            protocol: crate::session::PROTOCOL,
            supported_protocols: crate::session::supported_protocols(&crate::session::Version::this_system()),
        }
    }
}

/// Routine updates retain every protocol this build supports.
pub fn channel(policy: Policy, version: &Version, protocols: &[u16]) -> String {
    if policy != Policy::InstallMinorAndPatch {
        return "stable".to_owned();
    }
    let mut protocols = protocols.to_vec();
    protocols.sort_unstable();
    protocols.dedup();
    format!(
        "major-{}-protocols-{}",
        version.major,
        protocols.iter().map(u16::to_string).collect::<Vec<_>>().join("-")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failed_relaunch_save_retains_the_continuation_for_retry() {
        let mut pending = PendingRelaunch::default();
        pending.postpone("1.2.4".into(), "continuation");
        assert!(pending.retry(|_| anyhow::bail!("storage unavailable")).is_err());
        assert_eq!(
            pending
                .retry(|version| {
                    assert_eq!(version, "1.2.4");
                    Ok(())
                })
                .unwrap(),
            Some("continuation")
        );
        assert_eq!(pending.retry(|_| panic!("already resumed")).unwrap(), None);
        pending.postpone("1.2.4".into(), "cancelled");
        pending.clear();
        assert_eq!(pending.retry(|_| panic!("cancelled cycle")).unwrap(), None);
    }

    #[test]
    fn relaunch_restores_the_selected_profile_once_for_the_replacement() {
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("Daisy.app/Contents/MacOS/daisy");
        let launch = Launch {
            home: directory.path().join("selected-profile"),
            name: "Selected system".into(),
        };
        save_relaunch(directory.path(), &executable, "1.2.4", &launch, 100).unwrap();
        let restored = take_relaunch(directory.path(), &executable, "1.2.4", 101).unwrap();
        assert_eq!(restored, Some(launch.clone()));
        assert_eq!(
            restore_launch(None, None, restored),
            (Some(launch.home.clone()), Some(launch.name.clone()))
        );
        assert_eq!(
            take_relaunch(directory.path(), &executable, "1.2.4", 102).unwrap(),
            None
        );
    }

    #[test]
    fn relaunch_does_not_override_explicit_arguments_or_cross_bundles_or_releases() {
        let directory = tempfile::tempdir().unwrap();
        let executable = directory.path().join("Daisy.app/Contents/MacOS/daisy");
        let launch = Launch {
            home: directory.path().join("selected-profile"),
            name: "Selected system".into(),
        };
        let explicit = directory.path().join("explicit-profile");
        assert_eq!(
            restore_launch(Some(explicit.clone()), None, Some(launch.clone())),
            (Some(explicit), None)
        );
        assert_eq!(
            restore_launch(None, Some("Chosen name".into()), Some(launch.clone())),
            (Some(launch.home.clone()), Some("Chosen name".into()))
        );
        save_relaunch(directory.path(), &executable, "1.2.4", &launch, 100).unwrap();
        assert_eq!(
            take_relaunch(directory.path(), &directory.path().join("other"), "1.2.4", 101).unwrap(),
            None
        );
        assert_eq!(
            take_relaunch(directory.path(), &executable, "1.2.5", 101).unwrap(),
            None
        );
        for now in [99, 401] {
            save_relaunch(directory.path(), &executable, "1.2.4", &launch, 100).unwrap();
            assert_eq!(
                take_relaunch(directory.path(), &executable, "1.2.4", now).unwrap(),
                None
            );
        }
    }
    #[test]
    fn routine_channel_requires_all_supported_protocols() {
        assert_eq!(
            channel(Policy::InstallMinorAndPatch, &Version::new(0, 10, 0), &[7, 6, 7]),
            "major-0-protocols-6-7"
        );
        assert_eq!(channel(Policy::InstallAll, &Version::new(0, 10, 0), &[6, 7]), "stable");
        assert_eq!(channel(Policy::NotifyOnly, &Version::new(0, 10, 0), &[6, 7]), "stable");
    }
    #[test]
    fn last_check_survives_a_restart() {
        let home = tempfile::tempdir().unwrap();
        let last = LastCheck {
            at: 42,
            available: Some("0.11.0".to_owned()),
        };
        save_last_check(home.path(), &last).unwrap();
        assert_eq!(load_last_check(home.path()), Some(last));
    }
}
