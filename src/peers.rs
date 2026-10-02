//! Peers this system has paired with, pinned by public key, and how long each
//! stays trusted.
//!
//! The file is the only copy. Every change locks it, reads it, and writes it
//! back, so a running Daisy never restores a peer that another process
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

/// How often a running session renews its peer's last seen time. Well under
/// `ONCE_GRACE`, so another process never prunes a "once" peer mid-session.
const RENEW_EVERY: Duration = Duration::from_secs(20);
/// How often a running session checks that its peer is still trusted.
const CHECK_EVERY: Duration = Duration::from_secs(1);
const _: () = assert!(RENEW_EVERY.as_secs() * 2 < ONCE_GRACE.as_secs());

const HEADER: &str = "# Peers this system trusts. Change with `daisy trust` and `daisy forget`.\n\n";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Peer {
    pub name: String,
    pub key: PublicKey,
    pub policy: Policy,
    pub paired_at: Timestamp,
    pub last_seen: Timestamp,
    pub side: crate::input::Side,
    /// When `side` was chosen; the later choice wins when two systems disagree.
    pub side_chosen: Timestamp,
    /// The key it signs introductions and revocations with, once known.
    pub signing: Option<[u8; 32]>,
    /// The member that introduced it, for a peer this system never paired
    /// with itself. Trust in it ends with trust in that member.
    pub introduced_by: Option<PublicKey>,
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
    /// Every peer no longer trusted, including those they introduced.
    pub keys: Vec<PublicKey>,
    /// Selectors that matched no peer.
    pub unmatched: Vec<String>,
}

/// Peers, kept in `peers.toml`.
#[derive(Debug, Clone)]
pub struct PeerStore {
    path: PathBuf,
    lock: PathBuf,
    arrangement: PathBuf,
    revocations: PathBuf,
}

/// The most revocations kept; the oldest go first.
const MAX_REVOCATIONS: usize = 256;

#[derive(Default, Serialize, Deserialize)]
struct RevocationFile {
    /// Each a signed revocation, encoded as the wire carries it, in hex.
    #[serde(default)]
    revocation: Vec<String>,
}

/// Where every member of the group was last placed, as the newest
/// arrangement this system saw: `(version, author, offsets)`.
pub type Arrangement = (u64, PublicKey, Vec<(PublicKey, (f64, f64))>);

#[derive(Serialize, Deserialize)]
struct ArrangementFile {
    version: u64,
    author: String,
    #[serde(default, rename = "member")]
    members: Vec<Placed>,
}

#[derive(Serialize, Deserialize)]
struct Placed {
    key: String,
    x: f64,
    y: f64,
}

impl PeerStore {
    /// The peers and their screen relations in `home`.
    pub fn open(home: &Path) -> Result<Self> {
        Ok(Self {
            path: home.join("peers.toml"),
            lock: home.join("peers.lock"),
            arrangement: home.join("arrangement.toml"),
            revocations: home.join("revocations.toml"),
        })
    }

    /// Records the signing key `key` presented over an authenticated session.
    /// A key already recorded must not change.
    pub fn set_signing(&self, key: &PublicKey, signing: [u8; 32]) -> Result<()> {
        self.update(trust::now(), |peers| {
            match peers.iter_mut().find(|peer| peer.key == *key) {
                Some(peer) if peer.signing.is_some_and(|known| known != signing) => {
                    bail!("{} presented a different signing key", peer.name)
                }
                Some(peer) => {
                    peer.signing = Some(signing);
                    Ok(())
                }
                None => Ok(()),
            }
        })?
    }

    /// Trusts the newcomer in `introduction`, made by a member this system
    /// trusts, for no longer than it trusts that member. Returns whether it
    /// is newly trusted.
    pub fn introduce(&self, introduction: &crate::introduce::Introduction, now: Timestamp) -> Result<bool> {
        let revoked = self
            .revocations()
            .iter()
            .any(|signed| signed.body.revoked == introduction.newcomer && signed.body.at >= introduction.trusted_since);
        if revoked {
            return Ok(false);
        }
        let name = introduction.name.lines().next().unwrap_or("").trim().to_owned();
        self.update(now, |peers| {
            if peers.iter().any(|peer| peer.key == introduction.newcomer) {
                return false;
            }
            let Some(introducer) = peers.iter().find(|peer| peer.key == introduction.introducer) else {
                return false;
            };
            let policy = crate::introduce::stricter(introducer.policy, introduction.policy);
            // trust runs from when the introducer began it, so arriving late
            // never stretches it past the introducer's own
            peers.push(Peer {
                name,
                key: introduction.newcomer,
                policy,
                paired_at: introduction.trusted_since.min(now),
                last_seen: now,
                side: crate::input::Side::Right,
                side_chosen: 0,
                signing: Some(introduction.newcomer_signing),
                introduced_by: Some(introduction.introducer),
            });
            true
        })
    }

    /// Signs a revocation of each of `keys`, as forgotten here, and keeps it
    /// to send every member.
    pub fn record_revocations(
        &self,
        signer: &crate::introduce::Signer,
        me: PublicKey,
        keys: &[PublicKey],
        now: Timestamp,
    ) -> Result<()> {
        for revoked in keys {
            let revocation = crate::introduce::Revocation {
                by: me,
                revoked: *revoked,
                at: now,
            };
            self.revoke(
                crate::introduce::Signed::<crate::introduce::Revocation>::new(signer, revocation)?,
                now,
            )?;
        }
        Ok(())
    }

    /// Every revocation this system knows, oldest first.
    pub fn revocations(&self) -> Vec<crate::introduce::Signed<crate::introduce::Revocation>> {
        let Ok(text) = fs::read_to_string(&self.revocations) else {
            return Vec::new();
        };
        let file: RevocationFile = toml::from_str(&text).unwrap_or_default();
        file.revocation
            .iter()
            .filter_map(|hex| postcard::from_bytes(&from_hex(hex)?).ok())
            .collect()
    }

    /// Applies a verified revocation: keeps it, and stops trusting the
    /// system it names and every system that one introduced. Returns those
    /// systems, or `None` if this revocation was already known.
    pub fn revoke(
        &self,
        revocation: crate::introduce::Signed<crate::introduce::Revocation>,
        now: Timestamp,
    ) -> Result<Option<Vec<PublicKey>>> {
        let _lock = self.lock()?;
        let mut known = self.revocations();
        let superseded = known.iter().any(|signed| {
            signed.body.by == revocation.body.by
                && signed.body.revoked == revocation.body.revoked
                && signed.body.at >= revocation.body.at
        });
        if superseded {
            return Ok(None);
        }
        let (revoked, at) = (revocation.body.revoked, revocation.body.at);
        known.push(revocation);
        if known.len() > MAX_REVOCATIONS {
            known.drain(..known.len() - MAX_REVOCATIONS);
        }
        let file = RevocationFile {
            revocation: known
                .iter()
                .map(|signed| postcard::to_stdvec(signed).map(|bytes| to_hex(&bytes)))
                .collect::<std::result::Result<_, _>>()?,
        };
        let temporary = self.revocations.with_extension("tmp");
        fs::write(&temporary, toml::to_string(&file)?).with_context(|| format!("creating {}", temporary.display()))?;
        fs::rename(&temporary, &self.revocations).with_context(|| format!("saving {}", self.revocations.display()))?;
        let before: Vec<PublicKey> = self.read()?.iter().map(|peer| peer.key).collect();
        // trust given after the revocation, by pairing again, stands
        let after = self.change_locked(now, |peers| {
            peers.retain(|peer| peer.key != revoked || peer.paired_at > at)
        })?;
        Ok(Some(
            before
                .into_iter()
                .filter(|key| !after.iter().any(|peer| peer.key == *key))
                .collect(),
        ))
    }

    /// The arrangement last saved, if any can be read.
    pub fn arrangement(&self) -> Option<Arrangement> {
        let text = fs::read_to_string(&self.arrangement).ok()?;
        let file: ArrangementFile = toml::from_str(&text).ok()?;
        let author = PublicKey::from_hex(&file.author)?;
        let offsets = file
            .members
            .iter()
            .filter_map(|placed| Some((PublicKey::from_hex(&placed.key)?, (placed.x, placed.y))))
            .collect();
        Some((file.version, author, offsets))
    }

    /// Keeps `arrangement` for the next time the group meets.
    pub fn save_arrangement(&self, arrangement: &Arrangement) -> Result<()> {
        let (version, author, offsets) = arrangement;
        let file = ArrangementFile {
            version: *version,
            author: author.to_hex(),
            members: offsets
                .iter()
                .map(|(key, (x, y))| Placed {
                    key: key.to_hex(),
                    x: *x,
                    y: *y,
                })
                .collect(),
        };
        let text = toml::to_string(&file)?;
        let temporary = self.arrangement.with_extension("tmp");
        fs::write(&temporary, text).with_context(|| format!("creating {}", temporary.display()))?;
        fs::rename(&temporary, &self.arrangement).with_context(|| format!("saving {}", self.arrangement.display()))
    }

    pub fn list(&self, now: Timestamp) -> Result<Vec<Peer>> {
        self.update(now, |peers| peers.clone())
    }

    /// The peer with `key`, if this system still trusts it.
    pub fn trusted(&self, key: &PublicKey, now: Timestamp) -> Result<Option<Peer>> {
        Ok(self.list(now)?.into_iter().find(|peer| peer.key == *key))
    }

    /// Trust `key` under `name` and `policy`, replacing any earlier entry.
    pub fn pin(&self, key: PublicKey, name: &str, policy: Policy, now: Timestamp) -> Result<()> {
        // a name must not be able to smuggle anything else into the file
        let name = name.lines().next().unwrap_or("").trim().to_owned();
        self.update(now, |peers| {
            let signing = peers.iter().find(|peer| peer.key == key).and_then(|peer| peer.signing);
            peers.retain(|peer| peer.key != key);
            peers.push(Peer {
                name,
                key,
                policy,
                paired_at: now,
                last_seen: now,
                side: crate::input::Side::Right,
                side_chosen: 0,
                signing,
                introduced_by: None,
            });
        })
    }

    /// Record a session with `key` at `now`. Never adds a peer: returns
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

    /// Persist this system's half of the shared screen relation with a peer.
    /// Record a side chosen on this system; choosing the current side again
    /// changes nothing.
    pub fn set_side(&self, key: &PublicKey, side: crate::input::Side) -> Result<()> {
        let now = trust::now();
        self.update(now, |peers| {
            if let Some(peer) = peers.iter_mut().find(|peer| peer.key == *key && peer.side != side) {
                peer.side = side;
                peer.side_chosen = now;
            }
        })
    }

    /// Record the side both systems agreed on, and when it was chosen.
    pub fn agree_side(&self, key: &PublicKey, side: crate::input::Side, chosen: Timestamp) -> Result<()> {
        self.update(trust::now(), |peers| {
            if let Some(peer) = peers.iter_mut().find(|peer| peer.key == *key) {
                peer.side = side;
                peer.side_chosen = chosen;
            }
        })
    }

    /// Change the policy of every peer matching `selector`, counting it from
    /// `now` as if just paired. Returns how many matched, and those still
    /// trusted.
    pub fn set_policy(&self, selector: &str, policy: Policy, now: Timestamp) -> Result<(usize, Vec<Peer>)> {
        let matched = self.update(now, |peers| {
            let mut matched = Vec::new();
            for peer in peers.iter_mut().filter(|peer| peer.matches(selector)) {
                peer.policy = policy;
                peer.paired_at = now;
                peer.last_seen = now;
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

    /// Stop trusting every peer matching any of `selectors`, by name or
    /// fingerprint. A running session with one ends within `CHECK_EVERY`.
    pub fn forget(&self, selectors: &[String], now: Timestamp) -> Result<Forgotten> {
        let before: Vec<PublicKey> = self.list(now)?.iter().map(|peer| peer.key).collect();
        let (removed, unmatched) = self.update(now, |peers| {
            let count = peers.len();
            let unmatched = selectors
                .iter()
                .filter(|selector| !peers.iter().any(|peer| peer.matches(selector)))
                .cloned()
                .collect();
            peers.retain(|peer| !selectors.iter().any(|selector| peer.matches(selector)));
            (count - peers.len(), unmatched)
        })?;
        let after = self.list(now)?;
        Ok(Forgotten {
            removed,
            keys: before
                .into_iter()
                .filter(|key| !after.iter().any(|peer| peer.key == *key))
                .collect(),
            unmatched,
        })
    }

    /// Stop trusting every peer. Returns how many there were.
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

    /// Resolves, with the reason, once this system stops trusting `key` while a
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
                Err(error) => return error.context("checking this system's trust in the peer"),
            };
            match peer {
                None => return anyhow!("{name} was forgotten on this system"),
                Some(peer) if peer.is_expired(true, now) => {
                    return anyhow!("trust in {name} ended: it was trusted {}", peer.policy.describe());
                }
                Some(_) if renewed.elapsed() >= RENEW_EVERY => {
                    if let Err(error) = self.renew(&key, now) {
                        return error.context("renewing trust in the peer");
                    }
                    renewed = tokio::time::Instant::now();
                }
                Some(_) => {}
            }
        }
    }

    fn update<T>(&self, now: Timestamp, change: impl FnOnce(&mut Vec<Peer>) -> T) -> Result<T> {
        let _lock = self.lock()?;
        let mut result = None;
        self.change_locked(now, |peers| result = Some(change(peers)))?;
        Ok(result.expect("the change ran"))
    }

    /// Reads, changes and writes the peers while the caller holds the lock.
    /// Returns the peers as written.
    fn change_locked(&self, now: Timestamp, change: impl FnOnce(&mut Vec<Peer>)) -> Result<Vec<Peer>> {
        let before = self.read()?;
        // no session counts as live here: a running one keeps its own peer
        // fresh by renewing it well within the shortest window
        let prune = |peers: &mut Vec<Peer>| {
            peers.retain(|peer| !peer.is_expired(false, now));
            // trust in an introduced system ends with trust in its introducer
            loop {
                let count = peers.len();
                let present: Vec<PublicKey> = peers.iter().map(|peer| peer.key).collect();
                peers.retain(|peer| peer.introduced_by.is_none_or(|by| present.contains(&by)));
                if peers.len() == count {
                    break;
                }
            }
        };
        let mut peers = before.clone();
        prune(&mut peers);
        change(&mut peers);
        prune(&mut peers);
        if peers != before {
            self.write(&peers)?;
        }
        Ok(peers)
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
                    side: entry.side,
                    side_chosen: entry.side_chosen,
                    signing: entry
                        .signing
                        .as_deref()
                        .and_then(from_hex)
                        .and_then(|bytes| bytes.try_into().ok()),
                    introduced_by: entry.introduced_by.as_deref().and_then(PublicKey::from_hex),
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
                    side: peer.side,
                    side_chosen: peer.side_chosen,
                    signing: peer.signing.map(|signing| to_hex(&signing)),
                    introduced_by: peer.introduced_by.map(|key| key.to_hex()),
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

/// A running session with a peer. Dropping it records how the session
/// ended: a "once" peer is forgotten, unless the connection dropped
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
    side: crate::input::Side,
    side_chosen: Timestamp,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    signing: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    introduced_by: Option<String>,
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn from_hex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(text.get(index..index + 2)?, 16).ok())
        .collect()
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

    fn introduction(by: u8, newcomer: u8, trusted_since: Timestamp) -> crate::introduce::Introduction {
        crate::introduce::Introduction {
            introducer: key(by),
            newcomer: key(newcomer),
            newcomer_signing: [newcomer; 32],
            name: format!("system {newcomer}"),
            policy: Policy::Forever,
            trusted_since,
        }
    }

    fn revocation(by: u8, revoked: u8, at: Timestamp) -> crate::introduce::Signed<crate::introduce::Revocation> {
        crate::introduce::Signed {
            body: crate::introduce::Revocation {
                by: key(by),
                revoked: key(revoked),
                at,
            },
            signature: vec![0; 64],
        }
    }

    #[test]
    fn an_introduced_system_is_trusted_no_longer_than_its_introducer() {
        let (store, _dir) = store();
        store.pin(key(1), "laptop", Policy::Idle(5), NOW).unwrap();
        assert!(store.introduce(&introduction(1, 2, NOW), NOW).unwrap());
        let studio = store.trusted(&key(2), NOW).unwrap().unwrap();
        assert_eq!((studio.policy, studio.introduced_by), (Policy::Idle(5), Some(key(1))));
        assert_eq!(studio.signing, Some([2; 32]));
        // introduced again: nothing changes
        assert!(!store.introduce(&introduction(1, 2, NOW), NOW).unwrap());
        // trust in the introducer ends, and with it trust in what it introduced
        store.pin(key(3), "kept", Policy::Forever, NOW).unwrap();
        let later = NOW + 5 * 3600;
        let peers: Vec<_> = store.list(later).unwrap().iter().map(|peer| peer.key).collect();
        assert_eq!(peers, [key(3)]);
    }

    #[test]
    fn only_a_trusted_system_can_introduce() {
        let (store, _dir) = store();
        assert!(!store.introduce(&introduction(1, 2, NOW), NOW).unwrap());
        assert!(store.list(NOW).unwrap().is_empty());
    }

    #[test]
    fn forgetting_a_system_also_forgets_what_it_introduced() {
        let (store, _dir) = store();
        store.pin(key(1), "laptop", Policy::Forever, NOW).unwrap();
        store.introduce(&introduction(1, 2, NOW), NOW).unwrap();
        store.introduce(&introduction(2, 3, NOW), NOW).unwrap();
        let forgotten = store.forget(&["laptop".to_owned()], NOW).unwrap();
        assert_eq!(forgotten.removed, 1);
        assert_eq!(forgotten.keys, [key(1), key(2), key(3)]);
        assert!(store.list(NOW).unwrap().is_empty());
    }

    #[test]
    fn pairing_directly_outlives_the_introducer() {
        let (store, _dir) = store();
        store.pin(key(1), "laptop", Policy::Forever, NOW).unwrap();
        store.introduce(&introduction(1, 2, NOW), NOW).unwrap();
        store.pin(key(2), "studio", Policy::Forever, NOW).unwrap();
        store.forget(&["laptop".to_owned()], NOW).unwrap();
        assert_eq!(store.list(NOW).unwrap()[0].key, key(2));
    }

    #[test]
    fn a_revocation_removes_a_system_and_blocks_older_introductions_of_it() {
        let (store, _dir) = store();
        store.pin(key(1), "laptop", Policy::Forever, NOW).unwrap();
        store.introduce(&introduction(1, 2, NOW), NOW).unwrap();
        store.introduce(&introduction(2, 3, NOW), NOW).unwrap();
        let removed = store.revoke(revocation(1, 2, NOW + 10), NOW + 10).unwrap();
        assert_eq!(removed, Some(vec![key(2), key(3)]));
        assert_eq!(
            store.revoke(revocation(1, 2, NOW + 10), NOW + 10).unwrap(),
            None,
            "known already"
        );
        // a member that has not heard yet introduces it again, from before
        assert!(!store.introduce(&introduction(1, 2, NOW), NOW + 20).unwrap());
        // but a fresh pairing after the revocation counts
        assert!(store.introduce(&introduction(1, 2, NOW + 30), NOW + 30).unwrap());
        assert_eq!(store.revocations().len(), 1);
    }

    #[test]
    fn introduced_trust_runs_from_when_the_introducer_began_it() {
        let (store, _dir) = store();
        store.pin(key(1), "laptop", Policy::Forever, NOW).unwrap();
        let mut late = introduction(1, 2, NOW);
        late.policy = Policy::Days(7);
        // heard six days later, while catching up
        let heard = NOW + 6 * DAY;
        assert!(store.introduce(&late, heard).unwrap());
        assert!(store.trusted(&key(2), NOW + 7 * DAY - 1).unwrap().is_some());
        assert!(store.trusted(&key(2), NOW + 7 * DAY).unwrap().is_none());
    }

    #[test]
    fn an_old_revocation_never_undoes_a_later_pairing() {
        let (store, _dir) = store();
        store.pin(key(1), "laptop", Policy::Forever, NOW).unwrap();
        // forgotten at NOW + 10, paired again at NOW + 20 and introduced so
        store.introduce(&introduction(1, 2, NOW + 20), NOW + 30).unwrap();
        assert_eq!(
            store.revoke(revocation(1, 2, NOW + 10), NOW + 30).unwrap(),
            Some(vec![])
        );
        assert!(store.trusted(&key(2), NOW + 30).unwrap().is_some());
        // paired directly after the revocation, too
        store.pin(key(3), "desk", Policy::Forever, NOW + 40).unwrap();
        store.revoke(revocation(1, 3, NOW + 35), NOW + 40).unwrap();
        assert!(store.trusted(&key(3), NOW + 40).unwrap().is_some());
    }

    #[test]
    fn a_signing_key_once_known_cannot_change() {
        let (store, _dir) = store();
        store.pin(key(1), "laptop", Policy::Forever, NOW).unwrap();
        store.set_signing(&key(1), [1; 32]).unwrap();
        store.set_signing(&key(1), [1; 32]).unwrap();
        assert!(store.set_signing(&key(1), [2; 32]).is_err());
        // pairing again keeps it
        store.pin(key(1), "laptop", Policy::Forever, NOW).unwrap();
        assert_eq!(store.trusted(&key(1), NOW).unwrap().unwrap().signing, Some([1; 32]));
    }

    #[test]
    fn older_peer_files_load() {
        let (store, dir) = store();
        std::fs::write(
            dir.path().join("peers.toml"),
            format!(
                "[[peer]]\nkey = \"{}\"\nname = \"old\"\ntrust = \"idle\"\npaired_at = {NOW}\nlast_seen = {NOW}\nside = \"Right\"\nside_chosen = 0\n",
                key(1).to_hex()
            ),
        )
        .unwrap();
        let peer = store.trusted(&key(1), NOW).unwrap().unwrap();
        assert_eq!((peer.signing, peer.introduced_by), (None, None));
    }

    #[test]
    fn an_arrangement_is_kept_between_runs() {
        let (store, _dir) = store();
        assert_eq!(store.arrangement(), None);
        let saved = (42, key(1), vec![(key(1), (0.0, 0.0)), (key(2), (-1440.5, 120.0))]);
        store.save_arrangement(&saved).unwrap();
        assert_eq!(store.arrangement(), Some(saved));
    }

    #[test]
    fn paired_screen_relation_survives_reopening_and_renewal() {
        let (store, dir) = store();
        let now = trust::now();
        store.pin(key(1), "Studio", Policy::Forever, now).unwrap();
        store.set_side(&key(1), crate::input::Side::Above).unwrap();
        store.renew(&key(1), now).unwrap();
        let reopened = PeerStore::open(dir.path()).unwrap();
        assert_eq!(
            reopened.trusted(&key(1), now).unwrap().unwrap().side,
            crate::input::Side::Above
        );
    }

    #[test]
    fn choosing_the_same_side_again_keeps_when_it_was_chosen() {
        let (store, _dir) = store();
        let now = trust::now();
        store.pin(key(1), "Studio", Policy::Forever, now).unwrap();
        store.agree_side(&key(1), crate::input::Side::Left, 7).unwrap();
        store.set_side(&key(1), crate::input::Side::Left).unwrap();
        let peer = store.trusted(&key(1), now).unwrap().unwrap();
        assert_eq!((peer.side, peer.side_chosen), (crate::input::Side::Left, 7));
        store.set_side(&key(1), crate::input::Side::Above).unwrap();
        let peer = store.trusted(&key(1), now).unwrap().unwrap();
        assert_eq!(peer.side, crate::input::Side::Above);
        assert!(peer.side_chosen >= now);
    }

    #[test]
    fn missing_file_is_empty() {
        let (store, _dir) = store();
        assert!(store.list(NOW).unwrap().is_empty());
    }

    #[test]
    fn pinned_peers_survive_reopening() {
        let (store, dir) = store();
        store.pin(key(1), "Studio", Policy::Days(30), NOW).unwrap();

        let reopened = PeerStore::open(dir.path()).unwrap();
        let peer = reopened.trusted(&key(1), NOW).unwrap().unwrap();
        assert_eq!(peer.name, "Studio");
        assert_eq!(peer.policy, Policy::Days(30));
        assert_eq!((peer.paired_at, peer.last_seen), (NOW, NOW));
    }

    #[test]
    fn pinning_again_replaces_the_entry() {
        let (store, _dir) = store();
        store.pin(key(1), "old", Policy::Forever, NOW).unwrap();
        store.pin(key(1), "new", Policy::IDLE, NOW + 1).unwrap();

        let peers = store.list(NOW + 1).unwrap();
        assert_eq!(names(&peers), ["new"]);
        assert_eq!(peers[0].policy, Policy::IDLE);
    }

    #[test]
    fn name_cannot_inject_another_entry() {
        let (store, _dir) = store();
        let injected = format!("evil\"\n[[peer]]\nkey = \"{}\"\nname = \"trusted\"", key(2).to_hex());
        store.pin(key(1), &injected, Policy::IDLE, NOW).unwrap();

        assert_eq!(store.list(NOW).unwrap().len(), 1);
        assert!(store.trusted(&key(2), NOW).unwrap().is_none());
    }

    #[test]
    fn expired_trust_is_refused_and_dropped_from_the_file() {
        let (store, _dir) = store();
        store.pin(key(1), "idle", Policy::IDLE, NOW).unwrap();
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
        store.pin(key(1), "idle", Policy::IDLE, NOW).unwrap();
        store.pin(key(2), "deadline", Policy::Days(5), NOW).unwrap();
        for day in 1..=6 {
            let now = NOW + day * DAY;
            store.renew(&key(1), now).unwrap();
            store.renew(&key(2), now).unwrap();
        }
        assert_eq!(names(&store.list(NOW + 6 * DAY).unwrap()), ["idle"]);
    }

    #[test]
    fn renewing_never_adds_a_peer() {
        let (store, _dir) = store();
        assert!(!store.renew(&key(1), NOW).unwrap());
        assert!(store.list(NOW).unwrap().is_empty());
    }

    #[test]
    fn a_forgotten_peer_stays_forgotten_by_a_running_session() {
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
            store.pin(key(byte), name, Policy::IDLE, NOW).unwrap();
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
    fn a_new_policy_runs_from_when_it_was_set() {
        let (store, _dir) = store();
        store.pin(key(1), "studio", Policy::IDLE, NOW).unwrap();
        let (matched, still) = store.set_policy("studio", Policy::Forever, NOW + DAY).unwrap();
        assert_eq!((matched, still[0].policy), (1, Policy::Forever));

        // unused for ten days, then trusted until unused for five hours
        let changed = NOW + 10 * DAY;
        let (_, still) = store.set_policy("studio", Policy::Idle(5), changed).unwrap();
        assert_eq!(still.len(), 1);
        assert_eq!(store.list(changed + 5 * 3600 - 1).unwrap().len(), 1);
        assert!(store.list(changed + 5 * 3600).unwrap().is_empty());

        store.pin(key(2), "laptop", Policy::IDLE, changed - DAY).unwrap();
        let (_, still) = store.set_policy("laptop", Policy::Days(7), changed).unwrap();
        assert_eq!(still.len(), 1);
        assert!(store.list(changed + 7 * DAY).unwrap().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn watching_ends_when_the_peer_is_forgotten() {
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
    fn rejects_a_malformed_file() {
        let (store, dir) = store();
        fs::write(dir.path().join("peers.toml"), "[[peer]]\nkey = \"nope\"\n").unwrap();
        assert!(store.list(NOW).is_err());
    }
}
