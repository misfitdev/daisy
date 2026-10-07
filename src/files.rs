//! File-offer decisions and the optional bulk-stream contract. No native calls.

use std::collections::BTreeMap;
use std::path::{Component, Path};
use std::time::{Duration, Instant};

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

use crate::identity::PublicKey;

pub type OfferId = [u8; 16];
pub const TTL: Duration = Duration::from_secs(15);
pub const MAX_ITEMS: usize = 64;
pub const MAX_ENTRIES: usize = 100_000;
pub const MAX_BYTES: u64 = 4 << 30;
pub const CHUNK: usize = 16_000;
pub const MAX_ATTRIBUTES: usize = 64;
pub const MAX_ATTRIBUTE_BYTES: u64 = 32 << 20;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    pub name: String,
    pub directory: bool,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Offer {
    pub id: OfferId,
    pub port: u16,
    pub items: Vec<Item>,
}

impl Offer {
    pub fn validate(&self) -> Result<()> {
        ensure!(self.port != 0, "invalid file-transfer port");
        ensure!(
            !self.items.is_empty() && self.items.len() <= MAX_ITEMS,
            "too many copied files"
        );
        let mut total = 0u64;
        for item in &self.items {
            safe_name(&item.name)?;
            total = total
                .checked_add(item.bytes)
                .ok_or_else(|| anyhow::anyhow!("file size overflow"))?;
            ensure!(total <= MAX_BYTES, "copied files exceed the 4 GiB limit");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    File,
    Directory,
    Symlink { target: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Part {
    Entry {
        path: String,
        kind: Kind,
        len: u64,
        mode: u32,
        compressed: bool,
    },
    Data {
        bytes: Vec<u8>,
    },
    DataEnd,
    Attribute {
        name: String,
        len: u64,
    },
    EntryEnd,
    Finished,
    Failed {
        reason: String,
    },
}

pub fn safe_name(name: &str) -> Result<()> {
    ensure!(
        !name.is_empty() && name.len() <= 255 && !name.contains(['/', '\0']),
        "invalid copied file name"
    );
    ensure!(name != "." && name != "..", "invalid copied file name");
    Ok(())
}

pub fn safe_relative(path: &str) -> Result<()> {
    ensure!(path.len() <= 4096 && !path.contains('\0'), "invalid relative file path");
    ensure!(
        Path::new(path).components().count() <= 128,
        "copied folder is too deeply nested"
    );
    // Manifest keys must match filesystem paths; components() alone normalizes
    // repeated separators and internal dots before the link graph sees them.
    if !path.is_empty() {
        for name in path.split('/') {
            safe_name(name)?;
        }
    }
    // An empty path denotes the offered top-level item itself.
    ensure!(
        Path::new(path).components().all(|c| matches!(c, Component::Normal(_))),
        "file path escapes its offered item"
    );
    Ok(())
}

pub fn safe_link(path: &str, target: &str) -> Result<()> {
    ensure!(
        !target.is_empty() && target.len() <= 4096 && !target.contains('\0'),
        "invalid symbolic link"
    );
    let mut depth = Path::new(path).parent().map_or(0, |p| p.components().count());
    for component in Path::new(target).components() {
        match component {
            Component::Normal(_) => depth += 1,
            Component::CurDir => {}
            Component::ParentDir if depth > 0 => depth -= 1,
            _ => anyhow::bail!("symbolic link escapes its offered folder"),
        }
    }
    Ok(())
}

/// Resolve intermediate links against the complete manifest, rather than only
/// counting the target's lexical components. No filesystem reads are needed.
pub fn validate_links(links: &BTreeMap<String, String>) -> Result<()> {
    use std::collections::VecDeque;
    for (path, target) in links {
        safe_relative(path)?;
        safe_link(path, target)?;
        let mut resolved = Path::new(path)
            .parent()
            .into_iter()
            .flat_map(|p| p.components())
            .map(|c| c.as_os_str().to_owned())
            .collect::<Vec<_>>();
        let mut pending = Path::new(target)
            .components()
            .map(|c| c.as_os_str().to_owned())
            .collect::<VecDeque<_>>();
        let mut expansions = 0;
        while let Some(part) = pending.pop_front() {
            if part == "." {
                continue;
            }
            if part == ".." {
                ensure!(
                    resolved.pop().is_some(),
                    "symbolic link graph escapes its offered folder"
                );
                continue;
            }
            ensure!(
                Path::new(&part).components().all(|c| matches!(c, Component::Normal(_))),
                "invalid symbolic link graph"
            );
            resolved.push(part);
            let prefix: std::path::PathBuf = resolved.iter().collect();
            if let Some(next) = links.get(prefix.to_str().ok_or_else(|| anyhow::anyhow!("invalid link path"))?) {
                expansions += 1;
                ensure!(
                    expansions <= 40,
                    "symbolic link graph contains a cycle or too many links"
                );
                resolved.pop();
                for component in Path::new(next).components().rev() {
                    pending.push_front(component.as_os_str().to_owned());
                }
            }
        }
    }
    Ok(())
}

#[derive(Debug)]
struct Lease {
    until: Instant,
    active: usize,
}

/// One current offer. Replacing it prevents new requests; already acquired
/// transfers retain their own snapshot and may finish.
pub struct Leases {
    pub offer: Offer,
    destinations: BTreeMap<PublicKey, Lease>,
}

impl Leases {
    pub fn new(offer: Offer, peers: impl IntoIterator<Item = PublicKey>, now: Instant) -> Self {
        Self {
            offer,
            destinations: peers
                .into_iter()
                .map(|p| {
                    (
                        p,
                        Lease {
                            until: now + TTL,
                            active: 0,
                        },
                    )
                })
                .collect(),
        }
    }

    pub fn acquire(&mut self, peer: PublicKey, id: OfferId, item: u16, now: Instant) -> Result<()> {
        ensure!(id == self.offer.id, "unknown or superseded file offer");
        ensure!(usize::from(item) < self.offer.items.len(), "file item was not offered");
        let lease = self
            .destinations
            .get_mut(&peer)
            .ok_or_else(|| anyhow::anyhow!("file offer was not sent to this peer"))?;
        ensure!(lease.until > now, "file offer expired");
        ensure!(lease.active < 2, "too many simultaneous file requests");
        lease.active += 1;
        lease.until = now + TTL;
        Ok(())
    }

    pub fn touch(&mut self, peer: PublicKey, now: Instant) {
        if let Some(lease) = self.destinations.get_mut(&peer) {
            lease.until = now + TTL;
        }
    }

    pub fn finish(&mut self, peer: PublicKey, now: Instant) {
        if let Some(lease) = self.destinations.get_mut(&peer) {
            lease.active = lease.active.saturating_sub(1);
            lease.until = now + TTL;
        }
    }

    pub fn release(&mut self, peer: PublicKey) {
        self.destinations.remove(&peer);
    }

    pub fn expire(&mut self, now: Instant) {
        self.destinations
            .retain(|_, lease| lease.until > now || lease.active > 0);
    }

    pub fn empty(&self) -> bool {
        self.destinations.is_empty()
    }

    pub fn offered_to(&self, peer: PublicKey) -> bool {
        self.destinations.contains_key(&peer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symlink_graph_rejects_indirect_escape_and_cycles() {
        let links = |pairs: &[(&str, &str)]| pairs.iter().map(|(p, t)| (p.to_string(), t.to_string())).collect();
        assert!(validate_links(&links(&[("a", "."), ("b", "a/..")])).is_err());
        assert!(validate_links(&links(&[("a", "b"), ("b", "a")])).is_err());
        validate_links(&links(&[
            ("a", "nested"),
            ("b", "a/../file"),
            ("nested/link", "../file"),
        ]))
        .unwrap();
    }
    fn peer(byte: u8) -> PublicKey {
        PublicKey::from_bytes(&[byte; 32]).unwrap()
    }
    fn offer() -> Offer {
        Offer {
            id: [1; 16],
            port: 1234,
            items: vec![Item {
                name: "copied.txt".into(),
                directory: false,
                bytes: 12,
            }],
        }
    }

    #[test]
    fn only_offered_items_on_live_destination_leases_can_be_requested() {
        let now = Instant::now();
        let mut leases = Leases::new(offer(), [peer(1)], now);
        assert!(leases.acquire(peer(2), [1; 16], 0, now).is_err());
        assert!(leases.acquire(peer(1), [2; 16], 0, now).is_err());
        assert!(leases.acquire(peer(1), [1; 16], 1, now).is_err());
        leases.acquire(peer(1), [1; 16], 0, now).unwrap();
        leases.expire(now + TTL * 2);
        assert!(!leases.empty(), "active transfers must survive the idle TTL");
        assert!(
            leases.acquire(peer(1), [1; 16], 0, now + TTL * 2).is_err(),
            "a stalled transfer cannot authorize a new request"
        );
        leases.finish(peer(1), now + TTL * 2);
        leases.acquire(peer(1), [1; 16], 0, now + TTL * 2).unwrap();
        leases.finish(peer(1), now + TTL * 2);
        assert!(leases.acquire(peer(1), [1; 16], 0, now + TTL * 3).is_err());
        leases.release(peer(1));
        assert!(leases.acquire(peer(1), [1; 16], 0, now + TTL * 2).is_err());
    }

    #[test]
    fn optional_file_wire_tags_and_fields_are_fixed() {
        use crate::protocol::Message;
        let id = [7; 16];
        let mut released = vec![25];
        released.extend(id);
        let mut requested = vec![26];
        requested.extend(id);
        requested.push(0);
        let mut offered = vec![24];
        offered.extend(id);
        offered.extend([210, 9, 1, 1, b'a', 0, 3]);
        let cases = [
            (Message::FilesRelease { offer: id }, released),
            (Message::FilesRequest { offer: id, item: 0 }, requested),
            (
                Message::FilesOffer {
                    offer: Offer {
                        id,
                        port: 1234,
                        items: vec![Item {
                            name: "a".into(),
                            directory: false,
                            bytes: 3,
                        }],
                    },
                },
                offered,
            ),
            (
                Message::FilesPart {
                    part: Part::Entry {
                        path: "".into(),
                        kind: Kind::File,
                        len: 3,
                        mode: 0o600,
                        compressed: false,
                    },
                },
                vec![27, 0, 0, 0, 3, 128, 3, 0],
            ),
            (Message::FilesPart { part: Part::Finished }, vec![27, 5]),
        ];
        for (message, encoded) in cases {
            assert_eq!(postcard::to_stdvec(&message).unwrap(), encoded);
            assert_eq!(postcard::from_bytes::<Message>(&encoded).unwrap(), message);
        }
    }

    #[test]
    fn paths_and_symlinks_cannot_escape_the_offered_root() {
        for bad in ["../outside", "/absolute", "a/../../outside", "a\0b"] {
            assert!(safe_relative(bad).is_err(), "{bad}");
        }
        safe_relative("folder/file").unwrap();
        safe_relative("").unwrap();
        safe_link("Versions/Current", "A").unwrap();
        safe_link("nested/link", "../file").unwrap();
        for target in ["../../outside", "/outside"] {
            assert!(safe_link("nested/link", target).is_err());
        }
    }
    #[test]
    fn manifest_paths_reject_spelling_aliases_before_graph_validation() {
        for path in ["a//link", "a/./link", "a/", "./a"] {
            assert!(safe_relative(path).is_err(), "{path}");
        }
        safe_relative("a/link").unwrap();
        let links = [("a//link".into(), ".".into()), ("escape".into(), "a/link/../..".into())]
            .into_iter()
            .collect();
        assert!(validate_links(&links).is_err());
    }
}
