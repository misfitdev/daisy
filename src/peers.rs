//! Macs this one has paired with, pinned by public key, and how long each
//! stays trusted.
//!
//! The file is the only copy. Every change locks it, reads it, and writes it
//! back, so a running Daisy never restores a Mac that another process
//! forgot, and every change drops trust that has expired.

use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};

use crate::identity::PublicKey;
use crate::trust::{self, ONCE_GRACE, Policy, Timestamp};

/// How often a running session renews its Mac's last seen time. Well under
/// `ONCE_GRACE`, so another process never prunes a "once" Mac mid-session.
const RENEW_EVERY: Duration = Duration::from_secs(20);
/// How often a running session checks that its Mac is still trusted.
const CHECK_EVERY: Duration = Duration::from_secs(1);
const _: () = assert!(RENEW_EVERY.as_secs() * 2 < ONCE_GRACE.as_secs());

const HEADER: &str = "# Macs this one trusts. Change with `daisy trust` and `daisy forget`.\n\n";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Peer {
    pub name: String,
    pub key: PublicKey,
    pub policy: Policy,
    pub paired_at: Timestamp,
    pub last_seen: Timestamp,
}

impl Peer {
    /// When trust ends if no session is running, or `None` if never.
    pub fn expires_at(&self) -> Option<Timestamp> {
        self.policy.expires_at(self.paired_at, self.last_seen, false)
    }

    fn is_expired(&self, live: bool, now: Timestamp) -> bool {
        self.policy.is_expired(self.paired_at, self.last_seen, live, now)
    }

    fn matches(&self, selector: &str) -> bool {
        self.name == selector || self.key.fingerprint() == selector || self.key.to_hex() == selector
    }
}

/// The outcome of `forget`.
#[derive(Debug, PartialEq, Eq)]
pub struct Forgotten {
    pub removed: usize,
    /// Selectors that matched no paired Mac.
    pub unmatched: Vec<String>,
}

/// Paired Macs, kept in `peers.toml`.
#[derive(Debug, Clone)]
pub struct PeerStore {
    path: PathBuf,
    lock: PathBuf,
}

impl PeerStore {
    /// The store in `home`, moving over pairs from the earlier one line per
    /// Mac format, which trusted forever, under the default policy.
    pub fn open(home: &Path) -> Result<Self> {
        let store = Self {
            path: home.join("peers.toml"),
            lock: home.join("peers.lock"),
        };
        let legacy = home.join("peers");
        if legacy.exists() {
            let _lock = store.lock()?;
            if !store.path.exists() {
                let now = trust::now();
                let peers = read_legacy(&legacy)?
                    .into_iter()
                    .map(|(key, name)| Peer {
                        name,
                        key,
                        policy: Policy::default(),
                        paired_at: now,
                        last_seen: now,
                    })
                    .collect::<Vec<_>>();
                store.write(&peers)?;
            }
            fs::remove_file(&legacy).with_context(|| format!("removing {}", legacy.display()))?;
        }
        Ok(store)
    }

    /// Paired Macs still trusted at `now`, dropping any whose trust ended.
    pub fn list(&self, now: Timestamp) -> Result<Vec<Peer>> {
        self.update(now, |peers| peers.clone())
    }

    /// The paired Mac with `key`, if this Mac still trusts it.
    pub fn trusted(&self, key: &PublicKey, now: Timestamp) -> Result<Option<Peer>> {
        Ok(self.list(now)?.into_iter().find(|peer| peer.key == *key))
    }

    /// Trust `key` under `name` and `policy`, replacing any earlier entry.
    pub fn pin(&self, key: PublicKey, name: &str, policy: Policy, now: Timestamp) -> Result<()> {
        // a name must not be able to smuggle anything else into the file
        let name = name.lines().next().unwrap_or("").trim().to_owned();
        self.update(now, |peers| {
            peers.retain(|peer| peer.key != key);
            peers.push(Peer {
                name,
                key,
                policy,
                paired_at: now,
                last_seen: now,
            });
        })
    }

    /// Record a session with `key` at `now`. Never adds a Mac: returns
    /// whether it is still trusted.
    pub fn renew(&self, key: &PublicKey, now: Timestamp) -> Result<bool> {
        self.update(now, |peers| match peers.iter_mut().find(|peer| peer.key == *key) {
            Some(peer) => {
                peer.last_seen = now;
                true
            }
            None => false,
        })
    }

    /// Change the policy of every Mac matching `selector`. Returns how many
    /// matched, and those still trusted: one whose new policy has already
    /// run out is forgotten.
    pub fn set_policy(&self, selector: &str, policy: Policy, now: Timestamp) -> Result<(usize, Vec<Peer>)> {
        let matched = self.update(now, |peers| {
            let mut matched = Vec::new();
            for peer in peers.iter_mut().filter(|peer| peer.matches(selector)) {
                peer.policy = policy;
                matched.push(peer.key);
            }
            matched
        })?;
        let still = self
            .list(now)?
            .into_iter()
            .filter(|peer| matched.contains(&peer.key))
            .collect();
        Ok((matched.len(), still))
    }

    /// Stop trusting every Mac matching any of `selectors`, by name or
    /// fingerprint. A running session with one ends within `CHECK_EVERY`.
    pub fn forget(&self, selectors: &[String], now: Timestamp) -> Result<Forgotten> {
        self.update(now, |peers| {
            let before = peers.len();
            let unmatched = selectors
                .iter()
                .filter(|selector| !peers.iter().any(|peer| peer.matches(selector)))
                .cloned()
                .collect();
            peers.retain(|peer| !selectors.iter().any(|selector| peer.matches(selector)));
            Forgotten {
                removed: before - peers.len(),
                unmatched,
            }
        })
    }

    /// Stop trusting every Mac. Returns how many there were.
    pub fn forget_all(&self, now: Timestamp) -> Result<usize> {
        self.update(now, |peers| {
            let removed = peers.len();
            peers.clear();
            removed
        })
    }

    /// Mark a session with `key` as running, until the returned guard drops.
    pub fn visit(&self, key: PublicKey) -> Result<Visit<'_>> {
        if !self.renew(&key, trust::now())? {
            bail!("{key} is no longer trusted");
        }
        Ok(Visit {
            store: self,
            key,
            dropped: false,
        })
    }

    /// Resolves, with the reason, once this Mac stops trusting `key` while a
    /// session with it runs: it was forgotten, or its deadline passed.
    /// Renews its last seen time meanwhile.
    pub async fn watch(&self, key: PublicKey, name: &str) -> anyhow::Error {
        let mut check = tokio::time::interval(CHECK_EVERY);
        let mut renewed = tokio::time::Instant::now();
        loop {
            check.tick().await;
            let now = trust::now();
            let peer = match self.read() {
                Ok(peers) => peers.into_iter().find(|peer| peer.key == key),
                Err(error) => return error.context("checking this Mac's trust in the other"),
            };
            match peer {
                None => return anyhow!("{name} was forgotten on this Mac"),
                Some(peer) if peer.is_expired(true, now) => {
                    return anyhow!("trust in {name} ended: it was trusted {}", peer.policy.describe());
                }
                Some(_) if renewed.elapsed() >= RENEW_EVERY => {
                    if let Err(error) = self.renew(&key, now) {
                        return error.context("renewing trust in the other Mac");
                    }
                    renewed = tokio::time::Instant::now();
                }
                Some(_) => {}
            }
        }
    }

    fn update<T>(&self, now: Timestamp, change: impl FnOnce(&mut Vec<Peer>) -> T) -> Result<T> {
        let _lock = self.lock()?;
        let before = self.read()?;
        // no session counts as live here: a running one keeps its own Mac
        // fresh by renewing it well within the shortest window
        let unexpired = |peers: &mut Vec<Peer>| peers.retain(|peer| !peer.is_expired(false, now));
        let mut peers = before.clone();
        unexpired(&mut peers);
        let result = change(&mut peers);
        // a change of policy can end trust at once
        unexpired(&mut peers);
        if peers != before {
            self.write(&peers)?;
        }
        Ok(result)
    }

    fn lock(&self) -> Result<File> {
        let file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .open(&self.lock)
            .with_context(|| format!("opening {}", self.lock.display()))?;
        file.lock()
            .with_context(|| format!("locking {}", self.lock.display()))?;
        Ok(file)
    }

    fn read(&self) -> Result<Vec<Peer>> {
        let text = match fs::read_to_string(&self.path) {
            Ok(text) => text,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error).with_context(|| format!("reading {}", self.path.display())),
        };
        let file: PeerFile = toml::from_str(&text).with_context(|| format!("reading {}", self.path.display()))?;
        file.peers
            .into_iter()
            .map(|entry| {
                let key = PublicKey::from_hex(&entry.key)
                    .ok_or_else(|| anyhow!("{}: {:?} is not a public key", self.path.display(), entry.key))?;
                Ok(Peer {
                    name: entry.name,
                    key,
                    policy: entry.trust,
                    paired_at: entry.paired_at,
                    last_seen: entry.last_seen,
                })
            })
            .collect()
    }

    fn write(&self, peers: &[Peer]) -> Result<()> {
        let file = PeerFile {
            peers: peers
                .iter()
                .map(|peer| Entry {
                    key: peer.key.to_hex(),
                    name: peer.name.clone(),
                    trust: peer.policy,
                    paired_at: peer.paired_at,
                    last_seen: peer.last_seen,
                })
                .collect(),
        };
        let text = format!("{HEADER}{}", toml::to_string(&file)?);

        // write then rename, so a crash never leaves a half-written file
        let temporary = self.path.with_extension("tmp");
        let mut out = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temporary)
            .with_context(|| format!("creating {}", temporary.display()))?;
        out.write_all(text.as_bytes())?;
        out.sync_all()?;
        fs::rename(&temporary, &self.path).with_context(|| format!("saving {}", self.path.display()))?;
        Ok(())
    }
}

/// A running session with a paired Mac. Dropping it records how the session
/// ended: a "once" Mac is forgotten, unless the connection dropped
/// unexpectedly, which leaves `ONCE_GRACE` to reconnect.
pub struct Visit<'a> {
    store: &'a PeerStore,
    key: PublicKey,
    dropped: bool,
}

impl Visit<'_> {
    /// The connection was lost rather than closed on purpose.
    pub fn dropped(&mut self) {
        self.dropped = true;
    }
}

impl Drop for Visit<'_> {
    fn drop(&mut self) {
        let (key, dropped, now) = (self.key, self.dropped, trust::now());
        let result = self.store.update(now, |peers| {
            if let Some(index) = peers.iter().position(|peer| peer.key == key) {
                if peers[index].policy == Policy::Once && !dropped {
                    peers.remove(index);
                } else {
                    peers[index].last_seen = now;
                }
            }
        });
        if let Err(error) = result {
            tracing::warn!("recording the end of the session with {key}: {error:#}");
        }
    }
}

#[derive(Serialize, Deserialize)]
struct PeerFile {
    #[serde(default, rename = "peer")]
    peers: Vec<Entry>,
}

#[derive(Serialize, Deserialize)]
struct Entry {
    key: String,
    name: String,
    trust: Policy,
    paired_at: Timestamp,
    last_seen: Timestamp,
}

fn read_legacy(path: &Path) -> Result<Vec<(PublicKey, String)>> {
    let text = fs::read_to_string(path).with_context(|| format!("reading {}", path.display()))?;
    let mut peers = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (hex, name) = line.split_once(' ').unwrap_or((line, ""));
        let key =
            PublicKey::from_hex(hex).ok_or_else(|| anyhow!("{}:{}: not a public key", path.display(), index + 1))?;
        peers.push((key, name.trim().to_owned()));
    }
    Ok(peers)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::KEY_LEN;
    use crate::trust::IDLE_LIMIT;

    const NOW: Timestamp = 1_000_000_000;
    const DAY: u64 = 24 * 60 * 60;

    fn key(byte: u8) -> PublicKey {
        PublicKey::from_bytes(&[byte; KEY_LEN]).unwrap()
    }

    fn store() -> (PeerStore, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        (PeerStore::open(dir.path()).unwrap(), dir)
    }

    fn names(peers: &[Peer]) -> Vec<&str> {
        peers.iter().map(|peer| peer.name.as_str()).collect()
    }

    #[test]
    fn missing_file_is_empty() {
        let (store, _dir) = store();
        assert!(store.list(NOW).unwrap().is_empty());
    }

    #[test]
    fn pinned_peers_survive_reopening() {
        let (store, dir) = store();
        store.pin(key(1), "Studio Mac", Policy::Days(30), NOW).unwrap();

        let reopened = PeerStore::open(dir.path()).unwrap();
        let peer = reopened.trusted(&key(1), NOW).unwrap().unwrap();
        assert_eq!(peer.name, "Studio Mac");
        assert_eq!(peer.policy, Policy::Days(30));
        assert_eq!((peer.paired_at, peer.last_seen), (NOW, NOW));
    }

    #[test]
    fn pinning_again_replaces_the_entry() {
        let (store, _dir) = store();
        store.pin(key(1), "old", Policy::Forever, NOW).unwrap();
        store.pin(key(1), "new", Policy::Idle, NOW + 1).unwrap();

        let peers = store.list(NOW + 1).unwrap();
        assert_eq!(names(&peers), ["new"]);
        assert_eq!(peers[0].policy, Policy::Idle);
    }

    #[test]
    fn name_cannot_inject_another_entry() {
        let (store, _dir) = store();
        let injected = format!("evil\"\n[[peer]]\nkey = \"{}\"\nname = \"trusted\"", key(2).to_hex());
        store.pin(key(1), &injected, Policy::Idle, NOW).unwrap();

        assert_eq!(store.list(NOW).unwrap().len(), 1);
        assert!(store.trusted(&key(2), NOW).unwrap().is_none());
    }

    #[test]
    fn expired_trust_is_refused_and_dropped_from_the_file() {
        let (store, _dir) = store();
        store.pin(key(1), "idle", Policy::Idle, NOW).unwrap();
        store.pin(key(2), "deadline", Policy::Days(2), NOW).unwrap();
        store.pin(key(3), "forever", Policy::Forever, NOW).unwrap();
        let later = NOW + IDLE_LIMIT.as_secs();

        assert!(store.trusted(&key(1), later).unwrap().is_none());
        assert_eq!(names(&store.list(later).unwrap()), ["forever"]);
        // dropped for good, not hidden: going back in time does not restore it
        assert_eq!(names(&store.list(NOW).unwrap()), ["forever"]);
    }

    #[test]
    fn renewing_keeps_idle_trust_but_not_a_deadline() {
        let (store, _dir) = store();
        store.pin(key(1), "idle", Policy::Idle, NOW).unwrap();
        store.pin(key(2), "deadline", Policy::Days(5), NOW).unwrap();
        for day in 1..=6 {
            let now = NOW + day * DAY;
            store.renew(&key(1), now).unwrap();
            store.renew(&key(2), now).unwrap();
        }
        assert_eq!(names(&store.list(NOW + 6 * DAY).unwrap()), ["idle"]);
    }

    #[test]
    fn renewing_never_adds_a_mac() {
        let (store, _dir) = store();
        assert!(!store.renew(&key(1), NOW).unwrap());
        assert!(store.list(NOW).unwrap().is_empty());
    }

    #[test]
    fn a_forgotten_mac_stays_forgotten_by_a_running_session() {
        let (store, dir) = store();
        store.pin(key(1), "studio", Policy::Forever, NOW).unwrap();
        let visit = store.visit(key(1)).unwrap();

        // another process forgets it while the session runs
        PeerStore::open(dir.path())
            .unwrap()
            .forget(&["studio".to_owned()], NOW)
            .unwrap();
        store.renew(&key(1), NOW + 20).unwrap();
        drop(visit);

        assert!(store.list(NOW + 20).unwrap().is_empty());
    }

    #[test]
    fn once_trust_ends_with_a_session_closed_on_purpose() {
        let (store, _dir) = store();
        store.pin(key(1), "guest", Policy::Once, trust::now()).unwrap();
        drop(store.visit(key(1)).unwrap());
        assert!(store.trusted(&key(1), trust::now()).unwrap().is_none());
    }

    #[test]
    fn once_trust_allows_a_quick_reconnect_after_a_drop() {
        let (store, _dir) = store();
        store.pin(key(1), "guest", Policy::Once, trust::now()).unwrap();
        let mut visit = store.visit(key(1)).unwrap();
        visit.dropped();
        drop(visit);

        let dropped_at = store.list(trust::now()).unwrap()[0].last_seen;
        assert!(store.trusted(&key(1), dropped_at + 59).unwrap().is_some());
        assert!(store.trusted(&key(1), dropped_at + 60).unwrap().is_none());
    }

    #[test]
    fn forget_removes_one_many_or_all() {
        let (store, _dir) = store();
        for (byte, name) in [(1, "studio"), (2, "laptop"), (3, "mini"), (4, "spare")] {
            store.pin(key(byte), name, Policy::Idle, NOW).unwrap();
        }

        let one = store.forget(&["studio".to_owned()], NOW).unwrap();
        assert_eq!((one.removed, one.unmatched.len()), (1, 0));

        let selectors = [key(2).fingerprint(), "mini".to_owned(), "nobody".to_owned()];
        let many = store.forget(&selectors, NOW).unwrap();
        assert_eq!(many.removed, 2);
        assert_eq!(many.unmatched, ["nobody"]);

        assert_eq!(store.forget_all(NOW).unwrap(), 1);
        assert!(store.list(NOW).unwrap().is_empty());
    }

    #[test]
    fn changing_the_policy_can_end_trust_at_once() {
        let (store, _dir) = store();
        store.pin(key(1), "studio", Policy::Idle, NOW).unwrap();
        let (matched, still) = store.set_policy("studio", Policy::Forever, NOW + DAY).unwrap();
        assert_eq!((matched, still[0].policy), (1, Policy::Forever));

        let (matched, still) = store.set_policy("studio", Policy::Days(7), NOW + 10 * DAY).unwrap();
        assert_eq!((matched, still.len()), (1, 0));
        assert!(store.list(NOW + 10 * DAY).unwrap().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn watching_ends_when_the_mac_is_forgotten() {
        let (store, _dir) = store();
        store.pin(key(1), "studio", Policy::Forever, NOW).unwrap();
        let forget = async {
            tokio::time::sleep(Duration::from_secs(5)).await;
            store.forget_all(NOW).unwrap();
        };
        let (reason, ()) = tokio::join!(store.watch(key(1), "studio"), forget);
        assert!(reason.to_string().contains("forgotten"), "{reason}");
    }

    #[test]
    fn old_peer_files_move_over_under_the_default_policy() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("peers"),
            format!("# daisy paired peers\n{} studio mini\n", key(1).to_hex()),
        )
        .unwrap();

        let store = PeerStore::open(dir.path()).unwrap();
        let peers = store.list(trust::now()).unwrap();
        assert_eq!(names(&peers), ["studio mini"]);
        assert_eq!(peers[0].policy, Policy::Idle);
        assert!(!dir.path().join("peers").exists());
    }

    #[test]
    fn rejects_a_malformed_file() {
        let (store, dir) = store();
        fs::write(dir.path().join("peers.toml"), "[[peer]]\nkey = \"nope\"\n").unwrap();
        assert!(store.list(NOW).is_err());
    }
}
