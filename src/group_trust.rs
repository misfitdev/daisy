//! Replicated membership and retention choices. Admission is independent of
//! the member that announced it; policy edits cannot create a new admission.
use crate::{
    identity::PublicKey,
    trust::{Policy, Timestamp},
};
use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Stamp {
    pub version: u64,
    pub writer: [u8; 16],
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Active {
    /// Latest removal superseded by this fresh admission. Retained so stale
    /// pre-removal records stay invalid after offline catch-up.
    pub removed_through: Option<Timestamp>,
    pub signing: crate::device::PublicKey,
    pub name: String,
    pub policy: Policy,
    pub admitted_at: Timestamp,
    pub since: Timestamp,
    pub last_seen: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum State {
    Active(Active),
    Removed { at: Timestamp },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub member: PublicKey,
    pub stamp: Stamp,
    pub state: State,
}

impl Record {
    pub fn cutoff(&self) -> Option<Timestamp> {
        match &self.state {
            State::Active(active) => active.removed_through,
            State::Removed { at } => Some(*at),
        }
    }

    /// A removal remains effective even after fresh pairing replaces its
    /// tombstone with an active record. Policy edits never advance admission.
    pub fn merge(&self, incoming: &Self) -> Self {
        if self.member != incoming.member {
            return self.clone();
        }
        let cutoff = self.cutoff().max(incoming.cutoff());
        fn eligible(record: &Record, cutoff: Option<Timestamp>) -> Option<&Active> {
            match &record.state {
                State::Active(active) if cutoff.is_none_or(|at| active.admitted_at > at) => Some(active),
                _ => None,
            }
        }
        let old = eligible(self, cutoff);
        let next = eligible(incoming, cutoff);
        let mut result = match (old, next) {
            (Some(old), Some(next)) => {
                if old.signing != next.signing {
                    return self.clone();
                }
                let mut result = if incoming.stamp > self.stamp
                    || incoming.stamp == self.stamp
                        && (next.admitted_at, next.since, next.policy.to_string(), &next.name)
                            < (old.admitted_at, old.since, old.policy.to_string(), &old.name)
                {
                    incoming.clone()
                } else {
                    self.clone()
                };
                if let State::Active(active) = &mut result.state {
                    active.last_seen = old.last_seen.max(next.last_seen);
                    active.admitted_at = old.admitted_at.min(next.admitted_at);
                }
                result
            }
            (Some(_), None) => self.clone(),
            (None, Some(_)) => incoming.clone(),
            (None, None) => Self {
                member: self.member,
                stamp: Stamp::default(),
                state: State::Removed {
                    at: cutoff.expect("a removed admission has a cutoff"),
                },
            },
        };
        if let State::Active(active) = &mut result.state {
            active.removed_through = cutoff;
        }
        result
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Announcement {
    pub by: PublicKey,
    pub record: Record,
}

impl crate::introduce::Signed<Announcement> {
    pub fn new(signer: &crate::device::Signer, body: Announcement) -> Result<Self> {
        Ok(Self {
            signature: signer.sign_bytes(&bytes(&body)?)?,
            body,
        })
    }

    pub fn verify(&self, key: &crate::device::PublicKey, now: Timestamp) -> Result<&Announcement> {
        key.verify(&bytes(&self.body)?, &self.signature)?;
        let limit = now.saturating_add(300);
        if self.body.record.stamp.version > limit.saturating_mul(1000) {
            bail!("group choice is dated in the future");
        }
        match &self.body.record.state {
            State::Active(active) => {
                if active.admitted_at > limit
                    || active.since > limit
                    || active.last_seen > limit
                    || active.removed_through.is_some_and(|at| at >= active.admitted_at)
                {
                    bail!("group membership is dated in the future");
                }
                if active.name.len() > 1024 || active.name.contains(['\n', '\r']) {
                    bail!("invalid group member name");
                }
            }
            State::Removed { at } if *at > limit => bail!("group removal is dated in the future"),
            _ => {}
        }
        Ok(&self.body)
    }
}

fn bytes(body: &Announcement) -> Result<Vec<u8>> {
    let mut bytes = b"daisy group trust v1".to_vec();
    bytes.extend(postcard::to_stdvec(body)?);
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{device::Signer, peers::PeerStore};
    const NOW: u64 = 1_000_000_000;
    fn key(byte: u8) -> PublicKey {
        PublicKey::from_bytes(&[byte; 32]).unwrap()
    }
    fn active(member: u8, signing: crate::device::PublicKey, policy: Policy) -> Record {
        Record {
            member: key(member),
            stamp: Stamp::default(),
            state: State::Active(Active {
                removed_through: None,
                signing,
                name: "Peer".into(),
                policy,
                admitted_at: NOW,
                since: NOW,
                last_seen: NOW,
            }),
        }
    }
    fn replicate(from: &PeerStore, to: &PeerStore, me: u8, own: &Signer, now: u64) {
        for record in from.group_records().unwrap() {
            to.apply_group(&record, key(me), own.public(), now).unwrap();
        }
    }

    #[test]
    fn offline_catch_up_enforces_a_deadline_that_has_already_passed() {
        let home = tempfile::tempdir().unwrap();
        let store = PeerStore::open(home.path()).unwrap();
        let signer = Signer::generate().unwrap();
        let own = Signer::generate().unwrap();
        store
            .pin_device(key(9), signer.public(), "Peer", Policy::Forever, NOW)
            .unwrap();
        let mut record = active(9, signer.public(), Policy::Days(1));
        record.stamp.version = NOW * 1000;
        store.apply_group(&record, key(1), own.public(), NOW + 86401).unwrap();
        assert!(store.trusted(&key(9), NOW + 86401).unwrap().is_none());
    }

    #[test]
    fn removal_covers_an_admission_from_a_member_with_a_faster_clock() {
        let home = tempfile::tempdir().unwrap();
        let store = PeerStore::open(home.path()).unwrap();
        store
            .pin_device(
                key(9),
                Signer::generate().unwrap().public(),
                "Peer",
                Policy::Forever,
                NOW + 100,
            )
            .unwrap();
        store.forget(&["Peer".into()], NOW + 1).unwrap();
        assert!(matches!(store.group_records().unwrap()[0].state, State::Removed { at } if at >= NOW + 100));
    }

    #[test]
    fn leaving_discards_cached_members_before_joining_another_group() {
        let home = tempfile::tempdir().unwrap();
        let store = PeerStore::open(home.path()).unwrap();
        store
            .pin_device(
                key(9),
                Signer::generate().unwrap().public(),
                "Peer",
                Policy::Forever,
                NOW,
            )
            .unwrap();
        assert_eq!(store.forget_all(NOW + 1).unwrap(), 1);
        assert!(store.group_records().unwrap().is_empty());
        store
            .pin_device(
                key(8),
                Signer::generate().unwrap().public(),
                "New peer",
                Policy::Forever,
                NOW + 2,
            )
            .unwrap();
        assert_eq!(store.group_records().unwrap().len(), 1);
        assert_eq!(store.group_records().unwrap()[0].member, key(8));
    }

    #[test]
    fn choices_and_renewals_reach_offline_members_without_the_original_writer() {
        let homes: Vec<_> = (0..3).map(|_| tempfile::tempdir().unwrap()).collect();
        let stores: Vec<_> = homes.iter().map(|home| PeerStore::open(home.path()).unwrap()).collect();
        let signer = Signer::generate().unwrap();
        let own = Signer::generate().unwrap();
        for store in &stores {
            store
                .pin_device(key(9), signer.public(), "Peer", Policy::IDLE, NOW)
                .unwrap();
        }
        let stale = stores[2].group_records().unwrap().remove(0);
        stores[0].set_policy("Peer", Policy::Idle(2), NOW + 1).unwrap();
        replicate(&stores[0], &stores[1], 2, &own, NOW + 1);
        // Writer is no longer available; another member renews and relays.
        stores[1].renew(&key(9), NOW + 4000).unwrap();
        replicate(&stores[1], &stores[2], 3, &own, NOW + 4000);
        let peer = stores[2].trusted(&key(9), NOW + 8000).unwrap().unwrap();
        assert_eq!(peer.policy, Policy::Idle(2));
        assert_eq!(peer.paired_at, NOW + 1);
        assert_eq!(peer.last_seen, NOW + 4000);
        stores[2].apply_group(&stale, key(3), own.public(), NOW + 8000).unwrap();
        assert_eq!(
            stores[2].trusted(&key(9), NOW + 8000).unwrap().unwrap().policy,
            Policy::Idle(2)
        );
        assert_eq!(
            PeerStore::open(homes[2].path()).unwrap().group_records().unwrap(),
            stores[2].group_records().unwrap()
        );
    }

    #[test]
    fn a_policy_edit_cannot_resurrect_removed_members_but_fresh_pairing_can() {
        let a = tempfile::tempdir().unwrap();
        let b = tempfile::tempdir().unwrap();
        let a = PeerStore::open(a.path()).unwrap();
        let b = PeerStore::open(b.path()).unwrap();
        let signer = Signer::generate().unwrap();
        let own = Signer::generate().unwrap();
        for store in [&a, &b] {
            store
                .pin_device(key(9), signer.public(), "Peer", Policy::Forever, NOW)
                .unwrap();
        }
        a.forget(&["Peer".into()], NOW + 1).unwrap();
        b.set_policy("Peer", Policy::Idle(1), NOW + 2).unwrap();
        replicate(&b, &a, 1, &own, NOW + 2);
        assert!(a.trusted(&key(9), NOW + 2).unwrap().is_none());
        replicate(&a, &b, 2, &own, NOW + 2);
        assert!(b.trusted(&key(9), NOW + 2).unwrap().is_none());
        // Same-second re-pairing must still advance beyond the tombstone.
        let replacement = Signer::generate().unwrap();
        a.pin_device(key(9), replacement.public(), "Peer", Policy::Forever, NOW + 1)
            .unwrap();
        replicate(&a, &b, 2, &own, NOW + 2);
        assert_eq!(
            b.trusted(&key(9), NOW + 2).unwrap().unwrap().signing,
            Some(replacement.public())
        );
        // An offline copy sees only the fresh admission, without first receiving
        // its tombstone, then replays a pre-removal policy with a newer stamp.
        let c_home = tempfile::tempdir().unwrap();
        let c = PeerStore::open(c_home.path()).unwrap();
        c.pin_device(key(9), signer.public(), "Peer", Policy::Forever, NOW)
            .unwrap();
        let mut stale = c.group_records().unwrap().remove(0);
        stale.stamp.version = (NOW + 100) * 1000;
        replicate(&a, &c, 3, &own, NOW + 2);
        c.apply_group(&stale, key(3), own.public(), NOW + 2).unwrap();
        assert_eq!(
            c.trusted(&key(9), NOW + 2).unwrap().unwrap().signing,
            Some(replacement.public())
        );
        assert!(c.trusted(&key(9), NOW + 2).unwrap().unwrap().admitted_at > NOW + 1);
    }

    #[test]
    fn concurrent_choices_converge_and_renewal_never_undoes_a_choice() {
        let signer = Signer::generate().unwrap();
        let mut a = active(9, signer.public(), Policy::Idle(2));
        a.stamp = Stamp {
            version: NOW * 1000,
            writer: [1; 16],
        };
        let mut b = a.clone();
        b.stamp.writer = [2; 16];
        if let State::Active(active) = &mut b.state {
            active.policy = Policy::Forever;
        }
        let mut renewal = a.clone();
        renewal.stamp = Stamp::default();
        if let State::Active(active) = &mut renewal.state {
            active.last_seen += 100;
        }
        assert_eq!(a.merge(&b), b.merge(&a));
        assert_eq!(a.merge(&b).merge(&renewal), renewal.merge(&b).merge(&a));
        let State::Active(result) = a.merge(&b).merge(&renewal).state else {
            panic!()
        };
        assert_eq!(result.policy, Policy::Forever);
        assert_eq!(result.last_seen, NOW + 100);
    }

    #[test]
    fn joining_defaults_do_not_replace_established_group_choices_or_identity() {
        let signer = Signer::generate().unwrap();
        let mut established = active(9, signer.public(), Policy::Forever);
        established.stamp.version = NOW * 1000;
        let mut joining = active(9, signer.public(), Policy::IDLE);
        if let State::Active(active) = &mut joining.state {
            active.admitted_at += 1;
            active.since += 1;
        }
        assert_eq!(established.merge(&joining), joining.merge(&established));
        assert_eq!(established.merge(&joining), established);
        let mut impostor = joining;
        if let State::Active(active) = &mut impostor.state {
            active.signing = Signer::generate().unwrap().public();
        }
        assert_eq!(established.merge(&impostor), established);
    }

    #[test]
    fn signatures_bind_sender_membership_policy_and_time() {
        let signer = Signer::generate().unwrap();
        let signed = crate::introduce::Signed::<Announcement>::new(
            &signer,
            Announcement {
                by: key(1),
                record: active(9, signer.public(), Policy::IDLE),
            },
        )
        .unwrap();
        assert!(signed.verify(&signer.public(), NOW).is_ok());
        assert!(signed.verify(&Signer::generate().unwrap().public(), NOW).is_err());
        assert!(signed.verify(&signer.public(), NOW - 301).is_err());
        for change in [
            |body: &mut Announcement| body.by = key(2),
            |body: &mut Announcement| body.record.member = key(2),
            |body: &mut Announcement| body.record.stamp.version += 1,
            |body: &mut Announcement| body.record.state = State::Removed { at: NOW },
            |body: &mut Announcement| {
                if let State::Active(active) = &mut body.record.state {
                    active.policy = Policy::Forever;
                }
            },
        ] {
            let mut forged = signed.clone();
            change(&mut forged.body);
            assert!(forged.verify(&signer.public(), NOW).is_err());
        }
    }
}
