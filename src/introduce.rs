//! Signed introductions and revocations, which let a group grow from one
//! pairing.
//!
//! A system trusts whoever a member it trusts introduces, for no longer and
//! no more loosely than it trusts that member. Every member re-sends what
//! it knows whenever a link starts, so a system that was away catches up.
//! A compromised member can therefore admit a new system or remove one;
//! `docs/security-model.md` says so.

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use crate::identity::PublicKey;
use crate::trust::{Policy, Timestamp};

const INTRODUCTION: &[u8] = b"daisy introduction v1";
const REVOCATION: &[u8] = b"daisy revocation v1";
/// How far ahead of this system's clock a signed statement may be dated.
const CLOCK_SKEW: u64 = 5 * 60;

pub use crate::device::Signer;

/// "`introducer` trusts `newcomer`; trust it for no longer than `policy`."
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Introduction {
    pub introducer: PublicKey,
    pub newcomer: PublicKey,
    pub newcomer_signing: crate::device::PublicKey,
    pub name: String,
    pub policy: Policy,
    /// When the introducer began trusting the newcomer. A revocation dated
    /// later outranks it.
    pub trusted_since: Timestamp,
}

/// "`by` no longer trusts `revoked`, as of `at`."
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Revocation {
    pub by: PublicKey,
    pub revoked: PublicKey,
    pub at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Signed<T> {
    pub body: T,
    pub signature: Vec<u8>,
}

impl Signed<Introduction> {
    pub fn new(signer: &Signer, introduction: Introduction) -> Result<Self> {
        let signature = signer.sign_bytes(&signed_bytes(INTRODUCTION, &introduction)?)?;
        Ok(Self {
            body: introduction,
            signature,
        })
    }

    /// The introduction, if `introducer_signing` signed it and it is not
    /// dated in the future.
    pub fn verify(&self, introducer_signing: &crate::device::PublicKey, now: Timestamp) -> Result<&Introduction> {
        verify(INTRODUCTION, &self.body, &self.signature, introducer_signing)?;
        if self.body.trusted_since > now + CLOCK_SKEW {
            bail!("the introduction is dated in the future");
        }
        Ok(&self.body)
    }
}

impl Signed<Revocation> {
    pub fn new(signer: &Signer, revocation: Revocation) -> Result<Self> {
        let signature = signer.sign_bytes(&signed_bytes(REVOCATION, &revocation)?)?;
        Ok(Self {
            body: revocation,
            signature,
        })
    }

    pub fn verify(&self, by_signing: &crate::device::PublicKey, now: Timestamp) -> Result<&Revocation> {
        verify(REVOCATION, &self.body, &self.signature, by_signing)?;
        if self.body.at > now + CLOCK_SKEW {
            bail!("the revocation is dated in the future");
        }
        Ok(&self.body)
    }
}

fn signed_bytes(context: &[u8], body: &impl Serialize) -> Result<Vec<u8>> {
    let mut bytes = context.to_vec();
    bytes.extend(postcard::to_stdvec(body)?);
    Ok(bytes)
}

fn verify(context: &[u8], body: &impl Serialize, signature: &[u8], key: &crate::device::PublicKey) -> Result<()> {
    key.verify(&signed_bytes(context, body)?, signature)
}

/// The policy for a system introduced under `introduced`, by an introducer
/// this system trusts under `introducer`: whichever ends sooner.
pub fn stricter(introducer: Policy, introduced: Policy) -> Policy {
    use Policy::*;
    match (introducer, introduced) {
        (Once, _) | (_, Once) => Once,
        (Forever, other) | (other, Forever) => other,
        (Idle(a), Idle(b)) => Idle(a.min(b)),
        (Days(a), Days(b)) => Days(a.min(b)),
        (Idle(hours), Days(days)) | (Days(days), Idle(hours)) => Idle(hours.min(days.saturating_mul(24))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(byte: u8) -> PublicKey {
        PublicKey::from_bytes(&[byte; 32]).unwrap()
    }

    fn introduction() -> Introduction {
        Introduction {
            introducer: key(1),
            newcomer: key(2),
            newcomer_signing: Signer::generate().unwrap().public(),
            name: "Studio".to_owned(),
            policy: Policy::IDLE,
            trusted_since: 1000,
        }
    }

    #[test]
    fn an_introduction_verifies_only_with_the_introducers_key() {
        let introducer = Signer::generate().unwrap();
        let signed = Signed::<Introduction>::new(&introducer, introduction()).unwrap();
        assert_eq!(signed.verify(&introducer.public(), 1000).unwrap(), &signed.body);
        assert!(signed.verify(&Signer::generate().unwrap().public(), 1000).is_err());
    }

    #[test]
    fn a_tampered_introduction_is_refused() {
        let introducer = Signer::generate().unwrap();
        let signed = Signed::<Introduction>::new(&introducer, introduction()).unwrap();
        for tamper in [
            |i: &mut Introduction| i.newcomer = key(9),
            |i: &mut Introduction| i.newcomer_signing = crate::device::test_public(9),
            |i: &mut Introduction| i.policy = Policy::Forever,
            |i: &mut Introduction| i.trusted_since = 999,
            |i: &mut Introduction| i.name = "Other".to_owned(),
        ] {
            let mut forged = signed.clone();
            tamper(&mut forged.body);
            assert!(forged.verify(&introducer.public(), 1000).is_err());
        }
        let mut cut = signed.clone();
        cut.signature.pop();
        assert!(cut.verify(&introducer.public(), 1000).is_err());
    }

    #[test]
    fn a_statement_dated_in_the_future_is_refused() {
        let introducer = Signer::generate().unwrap();
        let signed = Signed::<Introduction>::new(&introducer, introduction()).unwrap();
        assert!(signed.verify(&introducer.public(), 1000 - CLOCK_SKEW).is_ok());
        assert!(signed.verify(&introducer.public(), 999 - CLOCK_SKEW).is_err());
        let revocation = Signed::<Revocation>::new(
            &introducer,
            Revocation {
                by: key(1),
                revoked: key(2),
                at: 1000,
            },
        )
        .unwrap();
        assert!(revocation.verify(&introducer.public(), 1000).is_ok());
        assert!(revocation.verify(&introducer.public(), 999 - CLOCK_SKEW).is_err());
    }

    #[test]
    fn an_introduction_cannot_pass_as_a_revocation() {
        let signer = Signer::generate().unwrap();
        let revocation = Revocation {
            by: key(1),
            revoked: key(2),
            at: 1000,
        };
        // the same signature over a different statement's bytes never verifies
        let introduction = Signed::<Introduction>::new(&signer, introduction()).unwrap();
        let forged = Signed {
            body: revocation,
            signature: introduction.signature,
        };
        assert!(forged.verify(&signer.public(), 1000).is_err());
    }

    #[test]
    fn introduced_trust_ends_no_later_than_the_introducers() {
        use Policy::*;
        assert_eq!(stricter(Once, Forever), Once);
        assert_eq!(stricter(Forever, Once), Once);
        assert_eq!(stricter(Forever, Forever), Forever);
        assert_eq!(stricter(Forever, Idle(5)), Idle(5));
        assert_eq!(stricter(Idle(5), Forever), Idle(5));
        assert_eq!(stricter(Idle(96), Idle(5)), Idle(5));
        assert_eq!(stricter(Days(30), Days(7)), Days(7));
        assert_eq!(stricter(Days(1), Idle(96)), Idle(24));
        assert_eq!(stricter(Idle(12), Days(30)), Idle(12));
    }

    #[test]
    fn a_signing_key_is_kept_between_runs_and_private() {
        use std::os::unix::fs::PermissionsExt;
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("signing");
        let first = Signer::load_or_create(&path).unwrap();
        let again = Signer::load_or_create(&path).unwrap();
        assert_eq!(first.public(), again.public());
        assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        std::fs::write(&path, b"short").unwrap();
        assert!(Signer::load_or_create(&path).is_err());
    }
}
