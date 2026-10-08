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

    pub fn permits(self, current: &Version, next: &Version) -> bool {
        next > current
            && next.pre.is_empty()
            && match self {
                Self::NotifyOnly => false,
                Self::InstallAll => true,
                Self::InstallMinorAndPatch => current.major == next.major,
            }
    }
}

/// The policy picker's items, in display order.
pub const ORDER: [Policy; 3] = [Policy::NotifyOnly, Policy::InstallMinorAndPatch, Policy::InstallAll];

/// Daisy's own repository, queried for its releases feed.
pub const REPOSITORY: &str = "misfitdev/daisy";

/// How often the background check repeats.
pub const INTERVAL: Duration = Duration::from_secs(6 * 60 * 60);

/// Anonymous HTTPS transport for the release installer. Redirects cannot
/// downgrade to HTTP, and request/body limits apply to every download.
pub struct ReleaseSource;

pub(crate) fn http_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build(),
        )
        .build()
        .new_agent()
}

impl crate::install::release::Source for ReleaseSource {
    fn get(&mut self, url: &str, limit: usize) -> Result<Vec<u8>> {
        http_agent()
            .get(url)
            .header("User-Agent", "daisy-release-installer")
            .header("Accept", "application/vnd.github+json")
            .config()
            .https_only(true)
            .max_redirects(5)
            .timeout_global(Some(Duration::from_secs(120)))
            .build()
            .call()
            .context("downloading release data")?
            .body_mut()
            .with_config()
            .limit(limit as u64)
            .read_to_vec()
            .context("reading bounded release data")
    }
}

/// Release compatibility metadata, bound to the final installation archive.
/// The installer must verify its release-workflow attestation before using it.
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

    /// Checks compatibility and archive binding, not provenance or whether to
    /// install. Release-number proximity is never evidence of compatibility.
    /// Unknown metadata and protocol changes require a manual group update.
    pub fn matches(&self, release: &Version, local_protocol: u16, archive: &[u8]) -> bool {
        let compatible = match self.format {
            1 => self.protocol == local_protocol,
            2 => {
                let mut supported = self.supported_protocols.clone();
                supported.sort_unstable();
                supported.dedup();
                !supported.is_empty()
                    && supported.len() == self.supported_protocols.len()
                    && supported.contains(&self.protocol)
                    && supported.contains(&local_protocol)
            }
            _ => false,
        };
        compatible
            && Version::parse(&self.version).is_ok_and(|version| version == *release)
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
    http_agent()
        .get(format!("https://api.github.com/repos/{repository}/releases"))
        .header("User-Agent", "daisy-update-check")
        .header("Accept", "application/vnd.github+json")
        .config()
        .timeout_global(Some(Duration::from_secs(30)))
        .build()
        .call()
        .context("requesting the releases feed")?
        .body_mut()
        .read_json::<Vec<Release>>()
        .context("reading the releases feed")
}

/// The result of one check of the releases feed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Checked {
    /// The newest release, when it is newer than this one.
    Found(Option<Release>),
    Failed,
}

/// Progress of the update checker.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckEvent {
    Started,
    Finished(Checked),
}

/// The last successful check, kept across launches so the Updates panel can
/// say when Daisy last looked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LastCheck {
    pub at: crate::trust::Timestamp,
    /// The newer release found then, if any.
    pub available: Option<String>,
}

impl LastCheck {
    pub fn of(found: Option<&Release>, at: crate::trust::Timestamp) -> Self {
        Self {
            at,
            available: found.map(|release| release.tag_name.trim_start_matches('v').to_owned()),
        }
    }
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

/// Checks the releases feed when `recheck` is notified and, while
/// `automatic` is on, at start and every `interval`; turning `automatic` on
/// checks immediately. Each check is announced as it starts and finishes,
/// including failures.
pub fn watch(
    repository: &'static str,
    current: Version,
    interval: Duration,
    automatic: watch::Receiver<bool>,
    recheck: std::sync::Arc<tokio::sync::Notify>,
) -> tokio::sync::mpsc::UnboundedReceiver<CheckEvent> {
    watch_with(move || check(repository), current, interval, automatic, recheck)
}

fn watch_with(
    fetch: impl Fn() -> Result<Vec<Release>> + Clone + Send + 'static,
    current: Version,
    interval: Duration,
    mut automatic: watch::Receiver<bool>,
    recheck: std::sync::Arc<tokio::sync::Notify>,
) -> tokio::sync::mpsc::UnboundedReceiver<CheckEvent> {
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    tokio::spawn(async move {
        let mut on = *automatic.borrow_and_update();
        let mut due = on;
        loop {
            if !due {
                tokio::select! {
                    () = tokio::time::sleep(interval), if on => {}
                    () = recheck.notified() => {}
                    changed = automatic.changed() => {
                        if changed.is_err() {
                            return;
                        }
                        let now = *automatic.borrow_and_update();
                        due = now && !on;
                        on = now;
                        continue;
                    }
                }
            }
            due = false;
            if sender.send(CheckEvent::Started).is_err() {
                return;
            }
            let current = current.clone();
            let fetch = fetch.clone();
            let checked = match tokio::task::spawn_blocking(fetch).await {
                Ok(Ok(releases)) => Checked::Found(newer_release(&current, &releases)),
                Ok(Err(error)) => {
                    tracing::warn!(error = ?error, "could not check for updates");
                    Checked::Failed
                }
                Err(error) => {
                    tracing::warn!(error = ?error, "update check task failed");
                    Checked::Failed
                }
            };
            if sender.send(CheckEvent::Finished(checked)).is_err() {
                return;
            }
        }
    });
    receiver
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(start_paused = true)]
    async fn checks_follow_the_automatic_switch_and_check_now() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = Arc::new(AtomicUsize::new(0));
        let counted = calls.clone();
        let fetch = move || {
            counted.fetch_add(1, Ordering::SeqCst);
            Ok(Vec::new())
        };
        let hour = Duration::from_secs(3600);
        let (switch, automatic) = watch::channel(false);
        let recheck = Arc::new(tokio::sync::Notify::new());
        let mut results = watch_with(fetch, Version::new(1, 0, 0), hour, automatic, recheck.clone());

        tokio::time::sleep(3 * hour).await;
        assert_eq!(calls.load(Ordering::SeqCst), 0, "off means no scheduled checks");

        recheck.notify_one();
        assert_eq!(
            results.recv().await,
            Some(CheckEvent::Started),
            "a check announces that it started"
        );
        assert_eq!(
            results.recv().await,
            Some(CheckEvent::Finished(Checked::Found(None))),
            "Check Now reports its result"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 1);

        switch.send_replace(true);
        results.recv().await.unwrap();
        results.recv().await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 2, "turning on checks immediately");
        tokio::time::sleep(hour).await;
        results.recv().await.unwrap();
        results.recv().await.unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 3, "on checks every interval");
    }

    #[test]
    fn status_line_says_what_was_found_and_when() {
        let ago = |at: u64| format!("at {at}");
        assert_eq!(status_line(None, false, ago), "Not checked yet.");
        assert_eq!(status_line(None, true, ago), "Could not check for updates.");
        let current = LastCheck { at: 5, available: None };
        assert_eq!(
            status_line(Some(&current), false, ago),
            "Daisy is up to date. Checked at 5."
        );
        assert_eq!(
            status_line(Some(&current), true, ago),
            "Could not check for updates. Last checked at 5."
        );
        let release = Release {
            tag_name: "v0.9.0".to_owned(),
            html_url: String::new(),
            draft: false,
            prerelease: false,
        };
        let newer = LastCheck::of(Some(&release), 7);
        assert_eq!(
            status_line(Some(&newer), false, ago),
            "Daisy 0.9.0 is available. Checked at 7."
        );
    }

    #[test]
    fn checking_line_grows_then_starts_over() {
        let lines: Vec<String> = (0..5).map(checking_line).collect();
        assert_eq!(
            lines,
            ["Checking", "Checking.", "Checking..", "Checking...", "Checking"]
        );
    }

    #[test]
    fn last_check_survives_a_restart() {
        let home = tempfile::tempdir().unwrap();
        assert_eq!(load_last_check(home.path()), None);
        let last = LastCheck {
            at: 1_791_000_000,
            available: Some("0.9.0".to_owned()),
        };
        save_last_check(home.path(), &last).unwrap();
        assert_eq!(load_last_check(home.path()), Some(last));
    }

    #[test]
    fn automatic_update_policies_follow_semver_boundaries() {
        let current = Version::parse("0.6.4").unwrap();
        assert!(!Policy::NotifyOnly.permits(&current, &Version::parse("0.6.5").unwrap()));
        assert!(Policy::InstallAll.permits(&current, &Version::parse("1.0.0").unwrap()));
        assert!(Policy::InstallMinorAndPatch.permits(&current, &Version::parse("0.7.0").unwrap()));
        assert!(!Policy::InstallMinorAndPatch.permits(&current, &Version::parse("1.0.0").unwrap()));
        assert!(!Policy::InstallAll.permits(&current, &Version::parse("0.6.5-rc.1").unwrap()));
    }

    #[test]
    fn compatibility_manifest_names_every_supported_protocol() {
        let archive = b"release bytes";
        let manifest = Manifest::for_archive(archive);
        let version = Version::parse(env!("CARGO_PKG_VERSION")).unwrap();
        assert!(manifest.matches(&version, crate::session::PROTOCOL, archive));
        assert!(manifest.matches(&version, crate::session::MIN_PROTOCOL, archive));
        assert!(!manifest.matches(&version, crate::session::MIN_PROTOCOL - 1, archive));
        let mut malformed = manifest.clone();
        malformed.supported_protocols.push(crate::session::PROTOCOL);
        assert!(!malformed.matches(&version, crate::session::PROTOCOL, archive));
        malformed = manifest;
        malformed.supported_protocols.clear();
        assert!(!malformed.matches(&version, crate::session::PROTOCOL, archive));
    }

    #[test]
    fn release_transport_refuses_plain_http_before_connecting() {
        use std::io::{Read, Write};
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        };
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let done = Arc::new(AtomicBool::new(false));
        let stop = done.clone();
        let server = std::thread::spawn(move || {
            while !stop.load(Ordering::Acquire) {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        stream.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
                        let mut request = [0; 2048];
                        let _ = stream.read(&mut request);
                        let _ =
                            stream.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nOK");
                        return true;
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(1))
                    }
                    Err(error) => panic!("{error}"),
                }
            }
            false
        });
        let result =
            crate::install::release::Source::get(&mut ReleaseSource, &format!("http://{address}/release"), 1024);
        done.store(true, Ordering::Release);
        let connected = server.join().unwrap();
        assert!(result.is_err());
        assert!(!connected);
    }

    #[test]
    fn release_metadata_binds_protocol_version_and_archive() {
        let archive = b"final signed release archive";
        let manifest = Manifest::for_archive(archive);
        let release = Version::parse(env!("CARGO_PKG_VERSION")).unwrap();
        let serialized = toml::to_string(&manifest).unwrap();
        let decoded: Manifest = toml::from_str(&serialized).unwrap();
        assert!(decoded.matches(&release, crate::session::PROTOCOL, archive));
        assert!(decoded.matches(&release, crate::session::MIN_PROTOCOL, archive));
        assert!(!decoded.matches(&release, crate::session::MIN_PROTOCOL - 1, archive));
        assert!(!decoded.matches(&release, crate::session::PROTOCOL + 1, archive));
        assert!(!decoded.matches(&release, crate::session::PROTOCOL, b"replaced archive"));
        assert!(!decoded.matches(&Version::parse("99.0.0").unwrap(), crate::session::PROTOCOL, archive));
        let mut unknown = decoded.clone();
        unknown.format = 3;
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
