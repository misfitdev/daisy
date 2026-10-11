//! Pure update settings, release compatibility channels, and status persistence.
use anyhow::{Context, Result};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
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
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let path = home.join(LAST_CHECK_FILE);
    let temporary = path.with_extension("tmp");
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&temporary)
        .with_context(|| format!("creating {}", temporary.display()))?;
    file.write_all(toml::to_string(last)?.as_bytes())?;
    file.sync_all()?;
    std::fs::rename(&temporary, &path).with_context(|| format!("saving {}", path.display()))
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
