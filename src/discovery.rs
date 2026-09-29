//! Finding paired Macs on the local network with Bonjour (DNS-SD).
//!
//! A Mac waiting for a connection advertises `_daisy._tcp` under a random
//! instance and host name. Its TXT record carries a fresh random nonce and a
//! short tag derived from its public key and that nonce. A Mac that paired
//! with it knows the key, so it can recognise the tag; anyone else learns
//! neither the Mac's name nor its key, and cannot link one advertisement to
//! the next. A Mac open to pairing also says so, so a new Mac can find it.
//!
//! The advertisement only says where to connect. Every connection still runs
//! the Noise handshake and the trust check, so a forged advertisement can at
//! most send a connection to the wrong place, where it fails.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use anyhow::{Context, Result};
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use rand_core::{OsRng, RngCore};
use sha2::{Digest, Sha256};

use crate::identity::PublicKey;

pub const SERVICE: &str = "_daisy._tcp.local.";
const VERSION: &str = "1";
const NONCE_LEN: usize = 16;
const TAG_LEN: usize = 8;
const TAG_LABEL: &[u8] = b"daisy beacon v1";

/// A tag a paired Mac can match against the key it pinned.
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

/// The TXT record for an advertisement.
pub fn properties(key: &PublicKey, nonce: &[u8; NONCE_LEN], pairing: bool) -> Vec<(&'static str, String)> {
    vec![
        ("v", VERSION.to_owned()),
        ("n", hex(nonce)),
        ("t", hex(&tag(key, nonce))),
        ("p", if pairing { "1" } else { "0" }.to_owned()),
    ]
}

/// What an advertisement turned out to be.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Seen {
    /// A Mac this one paired with.
    Paired(PublicKey),
    /// A Mac this one does not know, open to pairing now.
    Pairing,
}

/// Recognises an advertisement from its TXT values, given the keys of the
/// Macs this one still trusts. Anything else is ignored.
pub fn identify<'a>(txt: impl Fn(&str) -> Option<&'a str>, trusted: &[PublicKey]) -> Option<Seen> {
    if txt("v")? != VERSION {
        return None;
    }
    let nonce: [u8; NONCE_LEN] = unhex(txt("n")?)?.try_into().ok()?;
    let seen: [u8; TAG_LEN] = unhex(txt("t")?)?.try_into().ok()?;
    if let Some(key) = trusted.iter().find(|key| tag(key, &nonce) == seen) {
        return Some(Seen::Paired(*key));
    }
    (txt("p") == Some("1")).then_some(Seen::Pairing)
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
    OsRng.fill_bytes(&mut bytes);
    bytes
}

/// This Mac's advertisement, withdrawn when dropped.
pub struct Advertiser {
    daemon: ServiceDaemon,
    fullname: String,
}

impl Advertiser {
    pub fn start(key: &PublicKey, port: u16, pairing: bool) -> Result<Self> {
        let daemon = ServiceDaemon::new().context("starting Bonjour")?;
        // random names, so the advertisement does not reveal this Mac
        let id = hex(&random::<6>());
        let host = format!("daisy-{id}.local.");
        let properties = properties(key, &random(), pairing);
        let info = ServiceInfo::new(SERVICE, &format!("Daisy {id}"), &host, "", port, &properties[..])
            .context("describing the Bonjour advertisement")?
            .enable_addr_auto();
        let fullname = info.get_fullname().to_owned();
        daemon.register(info).context("advertising with Bonjour")?;
        Ok(Self { daemon, fullname })
    }
}

impl Drop for Advertiser {
    fn drop(&mut self) {
        let _ = self.daemon.unregister(&self.fullname);
        let _ = self.daemon.shutdown();
    }
}

/// A Mac found on the network.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub seen: Seen,
    pub address: SocketAddr,
}

/// Watches the network for Daisy advertisements, keeping the current set.
pub struct Browser {
    daemon: ServiceDaemon,
    events: mdns_sd::Receiver<ServiceEvent>,
    found: HashMap<String, Found>,
}

impl Browser {
    pub fn start() -> Result<Self> {
        let daemon = ServiceDaemon::new().context("starting Bonjour")?;
        let events = daemon.browse(SERVICE).context("browsing with Bonjour")?;
        Ok(Self {
            daemon,
            events,
            found: HashMap::new(),
        })
    }

    /// Waits for the next change and returns the Macs found so far, given
    /// the keys this Mac trusts now. `None` when browsing has stopped.
    pub async fn next(&mut self, trusted: &[PublicKey]) -> Option<Vec<Found>> {
        loop {
            match self.events.recv_async().await.ok()? {
                ServiceEvent::ServiceResolved(service) => {
                    let seen = identify(|key| service.get_property_val_str(key), trusted);
                    let address = pick_address(
                        service.get_addresses().iter().map(|ip| ip.to_ip_addr()),
                        service.get_port(),
                    );
                    match (seen, address) {
                        (Some(seen), Some(address)) => {
                            self.found
                                .insert(service.get_fullname().to_owned(), Found { seen, address });
                        }
                        _ => continue,
                    }
                }
                ServiceEvent::ServiceRemoved(_, fullname) => {
                    if self.found.remove(&fullname).is_none() {
                        continue;
                    }
                }
                ServiceEvent::SearchStopped(_) => return None,
                _ => continue,
            }
            return Some(self.found.values().cloned().collect());
        }
    }
}

impl Drop for Browser {
    fn drop(&mut self) {
        let _ = self.daemon.stop_browse(SERVICE);
        let _ = self.daemon.shutdown();
    }
}

/// Looks for the paired Mac with `key`, for up to `wait`.
pub async fn find(key: PublicKey, wait: Duration) -> Option<SocketAddr> {
    let mut browser = Browser::start().ok()?;
    tokio::time::timeout(wait, async {
        while let Some(found) = browser.next(&[key]).await {
            if let Some(mac) = found.iter().find(|mac| mac.seen == Seen::Paired(key)) {
                return Some(mac.address);
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

    fn lookup<'a>(props: &'a [(&'static str, String)]) -> impl Fn(&str) -> Option<&'a str> {
        |name| props.iter().find(|(k, _)| *k == name).map(|(_, v)| v.as_str())
    }

    #[test]
    fn a_paired_mac_is_recognised() {
        let (mine, other) = (key(), key());
        let props = properties(&mine, &[7; NONCE_LEN], false);
        assert_eq!(identify(lookup(&props), &[other, mine]), Some(Seen::Paired(mine)));
    }

    #[test]
    fn an_unknown_mac_is_ignored_unless_it_is_pairing() {
        let props = properties(&key(), &[7; NONCE_LEN], false);
        assert_eq!(identify(lookup(&props), &[key()]), None);
        let pairing = properties(&key(), &[7; NONCE_LEN], true);
        assert_eq!(identify(lookup(&pairing), &[key()]), Some(Seen::Pairing));
    }

    #[test]
    fn the_advertisement_reveals_neither_key_nor_name() {
        let mine = key();
        let props = properties(&mine, &[7; NONCE_LEN], true);
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
        let good = properties(&mine, &[7; NONCE_LEN], false);
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
        let _advertiser = Advertiser::start(&mine, 24999, false).unwrap();
        let found = find(mine, Duration::from_secs(8)).await;
        assert_eq!(found.map(|a| a.port()), Some(24999));
    }
}
