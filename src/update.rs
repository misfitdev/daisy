//! Whether a newer stable release of Daisy is available.
//!
//! `check` performs the request against the releases feed; `newer_release`
//! is the pure decision over its result, unit tested directly. `watch` ties
//! the two together on a timer so the rest of the app can read the current
//! answer without polling itself.

use std::time::Duration;

use anyhow::{Context, Result};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::sync::watch;

/// Daisy's own repository, queried for its releases feed.
pub const REPOSITORY: &str = "misfitdev/daisy";

/// How often the background check repeats.
pub const INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

/// Release compatibility metadata, bound to the final installation archive.
/// The installer must verify its release-workflow attestation before using it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub format: u32,
    pub version: String,
    pub protocol: u16,
    pub archive_sha256: String,
}

impl Manifest {
    pub fn for_archive(archive: &[u8]) -> Self {
        Self {
            format: 1,
            version: env!("CARGO_PKG_VERSION").to_owned(),
            protocol: crate::session::PROTOCOL,
            archive_sha256: archive_sha256(archive),
        }
    }

    /// Checks compatibility and archive binding, not provenance or whether to
    /// install. Release-number proximity is never evidence of compatibility.
    /// Unknown metadata and protocol changes require a manual group update.
    pub fn matches(&self, release: &Version, local_protocol: u16, archive: &[u8]) -> bool {
        self.format == 1
            && Version::parse(&self.version).is_ok_and(|version| version == *release)
            && self.protocol == local_protocol
            && self.archive_sha256 == archive_sha256(archive)
    }
}

fn archive_sha256(archive: &[u8]) -> String {
    Sha256::digest(archive)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Release {
    pub tag_name: String,
    pub html_url: String,
    #[serde(default)]
    pub draft: bool,
    #[serde(default)]
    pub prerelease: bool,
}

/// The newest stable release strictly newer than `current`, if any. Drafts,
/// prereleases and tags that do not parse as semver are never offered; a
/// draft in particular may have no build attached yet.
pub fn newer_release(current: &Version, releases: &[Release]) -> Option<Release> {
    releases
        .iter()
        .filter(|release| !release.draft && !release.prerelease)
        .filter_map(|release| Some((parse_tag(&release.tag_name)?, release)))
        .max_by(|(a, _), (b, _)| a.cmp(b))
        .filter(|(version, _)| *version > *current)
        .map(|(_, release)| release.clone())
}

fn parse_tag(tag: &str) -> Option<Version> {
    Version::parse(tag.strip_prefix('v').unwrap_or(tag)).ok()
}

/// Fetches the releases feed for `repository`. GitHub returns 403 for API
/// requests without a `User-Agent`.
pub fn check(repository: &str) -> Result<Vec<Release>> {
    ureq::get(format!("https://api.github.com/repos/{repository}/releases"))
        .header("User-Agent", "daisy-update-check")
        .header("Accept", "application/vnd.github+json")
        .call()
        .context("requesting the releases feed")?
        .body_mut()
        .read_json::<Vec<Release>>()
        .context("reading the releases feed")
}

/// Checks for a newer release every `interval`, starting immediately, and
/// publishes the newest one found. A failed check is logged and leaves the
/// last successful result in place rather than clearing it.
pub fn watch(repository: &'static str, current: Version, interval: Duration) -> watch::Receiver<Option<Release>> {
    let (sender, receiver) = watch::channel(None);
    tokio::spawn(async move {
        loop {
            let current = current.clone();
            match tokio::task::spawn_blocking(move || check(repository)).await {
                Ok(Ok(releases)) => {
                    if let Some(release) = newer_release(&current, &releases) {
                        sender.send_replace(Some(release));
                    }
                }
                Ok(Err(error)) => tracing::warn!(error = ?error, "could not check for updates"),
                Err(error) => tracing::warn!(error = ?error, "update check task failed"),
            }
            tokio::time::sleep(interval).await;
        }
    });
    receiver
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn release_metadata_binds_protocol_version_and_archive() {
        let archive = b"final signed release archive";
        let manifest = Manifest::for_archive(archive);
        let release = Version::parse(env!("CARGO_PKG_VERSION")).unwrap();
        let serialized = toml::to_string(&manifest).unwrap();
        let decoded: Manifest = toml::from_str(&serialized).unwrap();
        assert!(decoded.matches(&release, crate::session::PROTOCOL, archive));
        assert!(!decoded.matches(&release, crate::session::PROTOCOL - 1, archive));
        assert!(!decoded.matches(&release, crate::session::PROTOCOL + 1, archive));
        assert!(!decoded.matches(&release, crate::session::PROTOCOL, b"replaced archive"));
        assert!(!decoded.matches(&Version::parse("99.0.0").unwrap(), crate::session::PROTOCOL, archive));
        let mut unknown = decoded.clone();
        unknown.format = 2;
        assert!(!unknown.matches(&release, crate::session::PROTOCOL, archive));
        unknown = decoded;
        unknown.version = "latest".into();
        assert!(!unknown.matches(&release, crate::session::PROTOCOL, archive));
        assert!(toml::from_str::<Manifest>("").is_err());
        assert!(toml::from_str::<Manifest>(&format!("{serialized}\nunknown = true\n")).is_err());
    }

    fn release(tag: &str, draft: bool, prerelease: bool) -> Release {
        Release {
            tag_name: tag.to_owned(),
            html_url: format!("https://github.com/{REPOSITORY}/releases/tag/{tag}"),
            draft,
            prerelease,
        }
    }

    #[test]
    fn a_newer_stable_release_is_offered() {
        let current = Version::parse("0.6.0").unwrap();
        let releases = [release("v0.7.0", false, false)];
        assert_eq!(newer_release(&current, &releases), Some(releases[0].clone()));
    }

    #[test]
    fn the_current_or_an_older_release_is_not_offered() {
        let current = Version::parse("0.6.0").unwrap();
        assert_eq!(newer_release(&current, &[release("v0.6.0", false, false)]), None);
        assert_eq!(newer_release(&current, &[release("v0.5.0", false, false)]), None);
    }

    #[test]
    fn drafts_and_prereleases_are_never_offered() {
        let current = Version::parse("0.6.0").unwrap();
        let releases = [release("v0.7.0", true, false), release("v0.8.0-rc.1", false, true)];
        assert_eq!(newer_release(&current, &releases), None);
    }

    #[test]
    fn a_tag_that_does_not_parse_as_a_version_is_ignored() {
        let current = Version::parse("0.6.0").unwrap();
        assert_eq!(newer_release(&current, &[release("latest", false, false)]), None);
    }

    #[test]
    fn the_newest_of_several_newer_releases_is_offered() {
        let current = Version::parse("0.6.0").unwrap();
        let releases = [
            release("v0.7.0", false, false),
            release("v0.9.0", false, false),
            release("v0.8.0", false, false),
        ];
        assert_eq!(newer_release(&current, &releases), Some(releases[1].clone()));
    }
}
