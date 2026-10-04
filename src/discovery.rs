//! Finding peers on the local network with Bonjour (DNS-SD).
//!
//! A system waiting for a connection advertises `_daisy._tcp` under a random
//! instance and host name. Its TXT record carries a fresh random nonce and a
//! short tag derived from its public key and that nonce. A peer that paired
//! with it knows the key, so it can recognise the tag; anyone else learns
//! neither the system's name nor its key, and cannot link one advertisement to
//! the next. A system open to pairing also says so, so a new peer can find it.
//!
//! The advertisement only says where to connect. Every connection still runs
//! the Noise handshake and the trust check, so a forged advertisement can at
//! most send a connection to the wrong place, where it fails.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use anyhow::{Context, Result};
use getrandom::SysRng;
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use rand_core::{Rng, UnwrapErr};
use sha2::{Digest, Sha256};

use crate::identity::PublicKey;

pub const SERVICE: &str = "_daisy._tcp.local.";
const VERSION: &str = "1";
pub const NONCE_LEN: usize = 16;
const TAG_LEN: usize = 8;
const TAG_LABEL: &[u8] = b"daisy beacon v1";

/// How long a system the election passed over waits for the peer to connect
/// before connecting itself, for when the peer cannot see this system.
pub const OPENER_GRACE: Duration = Duration::from_secs(4);

/// A tag a peer can match against the key it pinned.
pub fn tag(key: &PublicKey, nonce: &[u8; NONCE_LEN]) -> [u8; TAG_LEN] {
    let digest = Sha256::new()
        .chain_update(TAG_LABEL)
        .chain_update(key.as_bytes())
        .chain_update(nonce)
        .finalize();
    let mut tag = [0; TAG_LEN];
    tag.copy_from_slice(&digest[..TAG_LEN]);
    tag
}

/// Whether a system is open to pairing, as its advertisement says.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Offer {
    Closed,
    /// Open since `since`, in Unix seconds. A `member` already has peers.
    Open {
        member: bool,
        since: u64,
    },
}

/// Two systems that opened pairing this close together, in seconds, are
/// told apart by their nonces instead; clocks are not that exact.
const SINCE_TIE: u64 = 2;

/// The TXT record for an advertisement.
pub fn properties(key: &PublicKey, nonce: &[u8; NONCE_LEN], offer: Offer) -> Vec<(&'static str, String)> {
    let mut properties = vec![
        ("v", VERSION.to_owned()),
        ("n", hex(nonce)),
        ("t", hex(&tag(key, nonce))),
    ];
    match offer {
        Offer::Closed => properties.push(("p", "0".to_owned())),
        Offer::Open { member, since } => {
            properties.push(("p", if member { "2" } else { "1" }.to_owned()));
            properties.push(("s", since.to_string()));
        }
    }
    properties
}

/// What an advertisement turned out to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Seen {
    /// A peer this system paired with.
    Paired(PublicKey),
    /// A system this one does not know, open to pairing now.
    Pairing { member: bool, since: u64 },
}

/// Recognises an advertisement from its TXT values, given the keys of the
/// peers this system still trusts. Anything else is ignored.
pub fn identify<'a>(txt: impl Fn(&str) -> Option<&'a str>, trusted: &[PublicKey]) -> Option<Seen> {
    if txt("v")? != VERSION {
        return None;
    }
    let nonce: [u8; NONCE_LEN] = unhex(txt("n")?)?.try_into().ok()?;
    let seen: [u8; TAG_LEN] = unhex(txt("t")?)?.try_into().ok()?;
    if let Some(key) = trusted.iter().find(|key| tag(key, &nonce) == seen) {
        return Some(Seen::Paired(*key));
    }
    let member = match txt("p")? {
        "1" => false,
        "2" => true,
        _ => return None,
    };
    let since = txt("s")?.parse().ok()?;
    Some(Seen::Pairing { member, since })
}

/// The address to connect to: IPv4 first, since it works on every network
/// Daisy runs on, and IPv6 only when there is nothing else.
pub fn pick_address(addresses: impl IntoIterator<Item = IpAddr>, port: u16) -> Option<SocketAddr> {
    let mut addresses: Vec<IpAddr> = addresses.into_iter().collect();
    addresses.sort_by_key(|ip| (!ip.is_ipv4(), *ip));
    addresses.first().map(|ip| SocketAddr::new(*ip, port))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn unhex(text: &str) -> Option<Vec<u8>> {
    if !text.len().is_multiple_of(2) {
        return None;
    }
    (0..text.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

fn random<const N: usize>() -> [u8; N] {
    let mut bytes = [0; N];
    UnwrapErr(SysRng).fill_bytes(&mut bytes);
    bytes
}

/// This system's advertisement, withdrawn when dropped.
pub struct Advertiser {
    daemon: ServiceDaemon,
    fullname: String,
    pub election: [u8; NONCE_LEN],
    /// Whether it says this system is open to pairing.
    pub offer: Offer,
}

impl Advertiser {
    pub fn start(key: &PublicKey, port: u16, offer: Offer) -> Result<Self> {
        let daemon = ServiceDaemon::new().context("starting Bonjour")?;
        // random names, so the advertisement does not reveal this system
        let id = hex(&random::<6>());
        let host = format!("daisy-{id}.local.");
        let election = random();
        let properties = properties(key, &election, offer);
        let info = ServiceInfo::new(SERVICE, &format!("Daisy {id}"), &host, "", port, &properties[..])
            .context("describing the Bonjour advertisement")?
            .enable_addr_auto();
        let fullname = info.get_fullname().to_owned();
        daemon.register(info).context("advertising with Bonjour")?;
        Ok(Self {
            daemon,
            fullname,
            election,
            offer,
        })
    }
}

impl Drop for Advertiser {
    fn drop(&mut self) {
        let _ = self.daemon.unregister(&self.fullname);
        let _ = self.daemon.shutdown();
    }
}

/// A system found on the network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub seen: Seen,
    pub address: SocketAddr,
    pub election: [u8; NONCE_LEN],
}

/// Paired keys elect one opener, which the other stands in for after a
/// grace period. For pairing, see `pairing_opener`. An equal key or nonce is
/// our own advertisement and never opens a connection.
pub fn opens_connection(
    own: PublicKey,
    election: Option<[u8; NONCE_LEN]>,
    found: &Found,
    offer: Offer,
    seen_for: Duration,
) -> bool {
    match (found.seen, offer) {
        (Seen::Paired(key), _) => own != key && (own.as_bytes() < key.as_bytes() || seen_for >= OPENER_GRACE),
        (Seen::Pairing { .. }, Offer::Closed) => false,
        (
            Seen::Pairing { member, since },
            Offer::Open {
                member: mine,
                since: my_since,
            },
        ) => match election {
            Some(nonce) if nonce == found.election => false,
            Some(nonce) => pairing_opener((mine, my_since, nonce), (member, since, found.election)),
            // not advertising, so the other system cannot find this one
            None => true,
        },
    }
}

/// Whether this system, rather than the other, opens a pairing connection,
/// given each side's `(member, since, nonce)`. The opener types the code and
/// the other shows it, so the code appears on the system just added: a
/// member opens to a newcomer, and otherwise the system open longer opens.
pub fn pairing_opener(mine: (bool, u64, [u8; NONCE_LEN]), theirs: (bool, u64, [u8; NONCE_LEN])) -> bool {
    let ((member, since, nonce), (their_member, their_since, their_nonce)) = (mine, theirs);
    if member != their_member {
        return member;
    }
    if since.abs_diff(their_since) > SINCE_TIE {
        return since < their_since;
    }
    nonce < their_nonce
}

/// The current time in Unix seconds, for `Offer::Open`.
pub fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs())
}

/// An advertisement as last heard: its TXT values and where to connect.
/// Kept raw so it can be recognised again when this system's trust changes.
#[derive(Debug, Clone)]
struct Heard {
    txt: HashMap<String, String>,
    address: SocketAddr,
}

/// What the heard advertisements are, given the keys this system trusts now.
/// A peer no longer trusted is dropped, or offered anonymously if it is open
/// to pairing.
fn classify<'a>(heard: impl IntoIterator<Item = &'a Heard>, trusted: &[PublicKey]) -> Vec<Found> {
    heard
        .into_iter()
        .filter_map(|heard| {
            let seen = identify(|key| heard.txt.get(key).map(String::as_str), trusted)?;
            Some(Found {
                seen,
                address: heard.address,
                election: unhex(heard.txt.get("n")?)?.try_into().ok()?,
            })
        })
        .collect()
}

/// Watches the network for Daisy advertisements, keeping the current set.
pub struct Browser {
    daemon: ServiceDaemon,
    events: mdns_sd::Receiver<ServiceEvent>,
    heard: HashMap<String, Heard>,
}

impl Browser {
    pub fn start() -> Result<Self> {
        let daemon = ServiceDaemon::new().context("starting Bonjour")?;
        let events = daemon.browse(SERVICE).context("browsing with Bonjour")?;
        Ok(Self {
            daemon,
            events,
            heard: HashMap::new(),
        })
    }

    /// The systems heard so far, recognised against `trusted`. Call again after
    /// trust changes; nothing is cached from earlier keys.
    pub fn current(&self, trusted: &[PublicKey]) -> Vec<Found> {
        classify(self.heard.values(), trusted)
    }

    /// Waits for the next advertisement to appear, change or go away.
    /// `false` when browsing has stopped.
    pub async fn changed(&mut self) -> bool {
        loop {
            let Ok(event) = self.events.recv_async().await else {
                return false;
            };
            match event {
                ServiceEvent::ServiceResolved(service) => {
                    let Some(address) = pick_address(
                        service.get_addresses().iter().map(|ip| ip.to_ip_addr()),
                        service.get_port(),
                    ) else {
                        continue;
                    };
                    let txt = ["v", "n", "t", "p"]
                        .into_iter()
                        .filter_map(|key| Some((key.to_owned(), service.get_property_val_str(key)?.to_owned())))
                        .collect();
                    self.heard
                        .insert(service.get_fullname().to_owned(), Heard { txt, address });
                }
                ServiceEvent::ServiceRemoved(_, fullname) => {
                    if self.heard.remove(&fullname).is_none() {
                        continue;
                    }
                }
                ServiceEvent::SearchStopped(_) => return false,
                _ => continue,
            }
            return true;
        }
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        let _ = self.daemon.stop_browse(SERVICE);
        let _ = self.daemon.shutdown();
    }
}

/// Looks for the peer with `key`, for up to `wait`.
pub async fn find(key: PublicKey, wait: Duration) -> Option<SocketAddr> {
    let mut browser = Browser::start().ok()?;
    tokio::time::timeout(wait, async {
        while browser.changed().await {
            if let Some(peer) = browser
                .current(&[key])
                .into_iter()
                .find(|peer| peer.seen == Seen::Paired(key))
            {
                return Some(peer.address);
            }
        }
        None
    })
    .await
    .ok()
    .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::Identity;

    fn key() -> PublicKey {
        Identity::generate().unwrap().public_key()
    }

    const OPEN: Offer = Offer::Open {
        member: false,
        since: 1_000,
    };

    #[test]
    fn paired_peers_elect_exactly_one_opener() {
        let (a, b) = (key(), key());
        let address = "127.0.0.1:24850".parse().unwrap();
        let peer_a = Found {
            seen: Seen::Paired(a),
            address,
            election: [1; NONCE_LEN],
        };
        let peer_b = Found {
            seen: Seen::Paired(b),
            address,
            election: [2; NONCE_LEN],
        };
        let now = Duration::ZERO;
        assert_ne!(
            opens_connection(a, None, &peer_b, Offer::Closed, now),
            opens_connection(b, None, &peer_a, Offer::Closed, now)
        );
        assert!(!opens_connection(a, None, &peer_a, Offer::Closed, now));
    }

    #[test]
    fn the_other_paired_system_connects_when_the_elected_one_does_not() {
        let (a, b) = (key(), key());
        let (low, high) = if a.as_bytes() < b.as_bytes() { (a, b) } else { (b, a) };
        let low_found = Found {
            seen: Seen::Paired(low),
            address: "127.0.0.1:24850".parse().unwrap(),
            election: [1; NONCE_LEN],
        };
        let just_under = OPENER_GRACE - Duration::from_millis(1);
        assert!(!opens_connection(high, None, &low_found, Offer::Closed, just_under));
        assert!(opens_connection(high, None, &low_found, Offer::Closed, OPENER_GRACE));
        assert!(!opens_connection(low, None, &low_found, Offer::Closed, OPENER_GRACE));
    }

    #[test]
    fn a_member_types_the_code_a_newcomer_shows() {
        let (member, newcomer) = ((true, 5_000, [9; NONCE_LEN]), (false, 1_000, [1; NONCE_LEN]));
        assert!(
            pairing_opener(member, newcomer),
            "even when the newcomer was open first"
        );
        assert!(!pairing_opener(newcomer, member));
    }

    #[test]
    fn of_two_newcomers_the_one_open_longer_types_the_code() {
        let (first, second) = ((false, 1_000, [9; NONCE_LEN]), (false, 1_030, [1; NONCE_LEN]));
        assert!(pairing_opener(first, second));
        assert!(!pairing_opener(second, first));
        let (a, b) = (
            (false, 1_000, [1; NONCE_LEN]),
            (false, 1_000 + SINCE_TIE, [2; NONCE_LEN]),
        );
        assert_ne!(
            pairing_opener(a, b),
            pairing_opener(b, a),
            "a near tie still elects one"
        );
    }

    #[test]
    fn pairing_needs_this_system_open_and_never_connects_to_itself() {
        let found = Found {
            seen: Seen::Pairing {
                member: false,
                since: 1_030,
            },
            address: "127.0.0.1:24850".parse().unwrap(),
            election: [2; NONCE_LEN],
        };
        let me = key();
        assert!(opens_connection(me, Some([1; NONCE_LEN]), &found, OPEN, Duration::ZERO));
        assert!(!opens_connection(
            me,
            Some([1; NONCE_LEN]),
            &found,
            Offer::Closed,
            OPENER_GRACE
        ));
        assert!(!opens_connection(me, Some([2; NONCE_LEN]), &found, OPEN, OPENER_GRACE));
        assert!(
            opens_connection(me, None, &found, OPEN, Duration::ZERO),
            "not advertising, so only this side can connect"
        );
    }

    fn lookup<'a>(props: &'a [(&'static str, String)]) -> impl Fn(&str) -> Option<&'a str> {
        |name| props.iter().find(|(k, _)| *k == name).map(|(_, v)| v.as_str())
    }

    #[test]
    fn a_paired_peer_is_recognised() {
        let (mine, other) = (key(), key());
        let props = properties(&mine, &[7; NONCE_LEN], Offer::Closed);
        assert_eq!(identify(lookup(&props), &[other, mine]), Some(Seen::Paired(mine)));
    }

    #[test]
    fn an_unknown_peer_is_ignored_unless_it_is_pairing() {
        let props = properties(&key(), &[7; NONCE_LEN], Offer::Closed);
        assert_eq!(identify(lookup(&props), &[key()]), None);
        let pairing = properties(&key(), &[7; NONCE_LEN], OPEN);
        assert_eq!(
            identify(lookup(&pairing), &[key()]),
            Some(Seen::Pairing {
                member: false,
                since: 1_000
            })
        );
        let member = Offer::Open {
            member: true,
            since: 1_000,
        };
        let pairing = properties(&key(), &[7; NONCE_LEN], member);
        assert!(matches!(
            identify(lookup(&pairing), &[key()]),
            Some(Seen::Pairing { member: true, .. })
        ));
    }

    #[test]
    fn the_advertisement_reveals_neither_key_nor_name() {
        let mine = key();
        let props = properties(&mine, &[7; NONCE_LEN], OPEN);
        let all: String = props.iter().map(|(k, v)| format!("{k}={v};")).collect();
        assert!(!all.contains(&mine.to_hex()));
        assert!(!all.contains(&mine.to_hex()[..16]));
    }

    #[test]
    fn a_new_nonce_makes_an_unlinkable_tag() {
        let mine = key();
        assert_ne!(tag(&mine, &[1; NONCE_LEN]), tag(&mine, &[2; NONCE_LEN]));
    }

    #[test]
    fn malformed_or_foreign_records_are_ignored() {
        let mine = key();
        let good = properties(&mine, &[7; NONCE_LEN], Offer::Closed);
        let mut wrong_version = good.clone();
        wrong_version[0].1 = "2".into();
        assert_eq!(identify(lookup(&wrong_version), &[mine]), None);
        let mut short_nonce = good.clone();
        short_nonce[1].1 = "abcd".into();
        assert_eq!(identify(lookup(&short_nonce), &[mine]), None);
        let mut not_hex = good.clone();
        not_hex[2].1 = "zz".repeat(TAG_LEN);
        assert_eq!(identify(lookup(&not_hex), &[mine]), None);
        assert_eq!(identify(|_| None, &[mine]), None);
    }

    #[test]
    fn ipv4_is_preferred() {
        let v6: IpAddr = "fe80::1".parse().unwrap();
        let v4: IpAddr = "192.168.1.20".parse().unwrap();
        assert_eq!(pick_address([v6, v4], 24850), Some(SocketAddr::new(v4, 24850)));
        assert_eq!(pick_address([v6], 24850), Some(SocketAddr::new(v6, 24850)));
        assert_eq!(pick_address([], 24850), None);
    }

    // Uses the real network stack: advertises and finds itself over mDNS.
    #[tokio::test]
    #[ignore = "uses the local network; run by hand with --ignored"]
    async fn an_advertisement_is_found_on_the_network() {
        let mine = key();
        let _advertiser = Advertiser::start(&mine, 24999, Offer::Closed).unwrap();
        let found = find(mine, Duration::from_secs(8)).await;
        assert_eq!(found.map(|a| a.port()), Some(24999));
    }

    fn heard(key: &PublicKey, pairing: bool, last: u8) -> Heard {
        let offer = if pairing { OPEN } else { Offer::Closed };
        Heard {
            txt: properties(key, &[last; NONCE_LEN], offer)
                .into_iter()
                .map(|(k, v)| (k.to_owned(), v))
                .collect(),
            address: SocketAddr::from(([192, 168, 1, last], 24850)),
        }
    }

    #[test]
    fn a_forgotten_peer_is_dropped_or_offered_only_for_pairing() {
        let (quiet, pairing) = (key(), key());
        let advertised = [heard(&quiet, false, 1), heard(&pairing, true, 2)];
        let trusted = classify(&advertised, &[quiet, pairing]);
        assert_eq!(trusted.iter().filter(|m| matches!(m.seen, Seen::Paired(_))).count(), 2);
        // both forgotten: the quiet one disappears, the pairing one loses its name
        let forgotten = classify(&advertised, &[]);
        assert_eq!(forgotten.len(), 1);
        assert!(matches!(forgotten[0].seen, Seen::Pairing { .. }));
        assert_eq!(forgotten[0].address, advertised[1].address);
    }
}
