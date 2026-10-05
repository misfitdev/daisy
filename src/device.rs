//! Device signatures. Production signing is performed only by the Secure Enclave.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::Path;

use anyhow::{Context, Result, bail};
use p256::ecdsa::{Signature, VerifyingKey, signature::Verifier};
use serde::{Deserialize, Serialize};

/// A compressed SEC1 P-256 public key, represented as its x coordinate and parity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicKey {
    x: [u8; 32],
    odd: bool,
}

impl PublicKey {
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        VerifyingKey::from_sec1_bytes(bytes).context("invalid device public key")?;
        if bytes.len() != 33 || !matches!(bytes[0], 2 | 3) {
            bail!("device public key must use compressed SEC1 encoding");
        }
        Ok(Self {
            x: bytes[1..].try_into()?,
            odd: bytes[0] == 3,
        })
    }

    pub fn to_bytes(self) -> [u8; 33] {
        let mut bytes = [0; 33];
        bytes[0] = if self.odd { 3 } else { 2 };
        bytes[1..].copy_from_slice(&self.x);
        bytes
    }

    pub fn verify(&self, message: &[u8], signature: &[u8]) -> Result<()> {
        let key = VerifyingKey::from_sec1_bytes(&self.to_bytes()).context("invalid device public key")?;
        let signature = Signature::from_der(signature).context("malformed device signature")?;
        key.verify(message, &signature)
            .context("device signature does not match")
    }
}

/// A Keychain reference to a system-specific Secure Enclave signing key.
/// The private scalar is never returned to Daisy.
pub struct Signer {
    reference: Vec<u8>,
    public: PublicKey,
}

impl Signer {
    pub fn generate() -> Result<Self> {
        let reference = backend::create()?;
        Self::from_reference(reference)
    }

    fn from_reference(reference: Vec<u8>) -> Result<Self> {
        let public = PublicKey::from_bytes(&backend::public(&reference)?)?;
        let signer = Self { reference, public };
        // Resolving the reference must also prove the key can sign here.
        let message = b"daisy device identity self check v1";
        signer.public.verify(message, &signer.sign_bytes(message)?)?;
        Ok(signer)
    }

    pub fn load_or_create(path: &Path) -> Result<Self> {
        let _lock = crate::identity::lock(path)?;
        match fs::read(path) {
            Ok(reference) => {
                if fs::metadata(path)?.permissions().mode() & 0o077 != 0 {
                    bail!("device identity is readable by other users; restrict it with chmod 600");
                }
                Self::from_reference(reference)
                    .context("device identity is unavailable on this system; reset the identity and pair again")
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let signer = Self::generate()?;
                signer.save(path)?;
                Ok(signer)
            }
            Err(error) => Err(error).context("reading device identity"),
        }
    }

    pub fn rotate(path: &Path) -> Result<Self> {
        Self::rotate_with_delete(path, backend::delete)
    }

    fn rotate_with_delete(path: &Path, delete: impl Fn(&[u8]) -> Result<()>) -> Result<Self> {
        let _lock = crate::identity::lock(path)?;
        let old = match fs::read(path) {
            Ok(reference) => Some(reference),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error).context("reading previous device identity"),
        };
        let signer = Self::generate()?;
        if let Err(error) = signer.save(path) {
            delete(&signer.reference).context("removing unsaved device identity")?;
            return Err(error);
        }
        if let Some(old) = old.as_deref()
            && let Err(error) = delete(old)
        {
            Self::save_reference(old, path).context("restoring previous device identity after failed rotation")?;
            delete(&signer.reference).context("removing replacement after failed rotation")?;
            return Err(error).context("retiring previous device identity; rotation cancelled");
        }
        Ok(signer)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        Self::save_reference(&self.reference, path)
    }

    fn save_reference(reference: &[u8], path: &Path) -> Result<()> {
        let temporary = path.with_extension("tmp");
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temporary)?;
        file.write_all(reference)?;
        file.sync_all()?;
        fs::rename(&temporary, path).context("saving device identity")
    }

    pub fn public(&self) -> PublicKey {
        self.public
    }

    pub fn sign_bytes(&self, message: &[u8]) -> Result<Vec<u8>> {
        backend::sign(&self.reference, message)
    }
}

#[cfg(all(target_os = "macos", not(test)))]
mod backend {
    pub use crate::macos::device::{create, delete, public, sign};
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[test]
    fn failed_key_retirement_restores_reference_and_reports_failure() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("device-identity");
        let original = Signer::load_or_create(&path).unwrap();
        let deleted = RefCell::new(Vec::new());
        let error = Signer::rotate_with_delete(&path, |reference| {
            deleted.borrow_mut().push(reference.to_vec());
            if reference == original.reference {
                bail!("injected retirement failure");
            }
            Ok(())
        })
        .err()
        .unwrap();
        assert!(format!("{error:#}").contains("injected retirement failure"));
        assert_eq!(fs::read(&path).unwrap(), original.reference);
        assert_eq!(Signer::load_or_create(&path).unwrap().public(), original.public());
        let deleted = deleted.borrow();
        assert_eq!(deleted.len(), 2);
        assert_eq!(deleted[0], original.reference);
        assert_ne!(deleted[1], original.reference);
    }
}

#[cfg(all(not(target_os = "macos"), not(test)))]
mod backend {
    use anyhow::{Result, bail};
    pub fn delete(_: &[u8]) -> Result<()> {
        bail!("Daisy requires a Secure Enclave on macOS")
    }
    pub fn create() -> Result<Vec<u8>> {
        bail!("Daisy requires a Secure Enclave on macOS")
    }
    pub fn public(_: &[u8]) -> Result<Vec<u8>> {
        bail!("Daisy requires a Secure Enclave on macOS")
    }
    pub fn sign(_: &[u8], _: &[u8]) -> Result<Vec<u8>> {
        bail!("Daisy requires a Secure Enclave on macOS")
    }
}

#[cfg(test)]
pub(crate) fn test_public(byte: u8) -> PublicKey {
    let mut seed = [0; 32];
    seed[30..].copy_from_slice(&(u16::from(byte) + 1).to_be_bytes());
    let key = p256::ecdsa::SigningKey::from_slice(&seed).unwrap();
    PublicKey::from_bytes(key.verifying_key().to_encoded_point(true).as_bytes()).unwrap()
}

// Decision tests use software keys; this backend does not exist in app builds.
#[cfg(test)]
mod backend {
    use anyhow::{Context, Result};
    use p256::ecdsa::{SigningKey, signature::Signer};
    pub fn delete(_: &[u8]) -> Result<()> {
        Ok(())
    }
    pub fn create() -> Result<Vec<u8>> {
        loop {
            let mut bytes = [0; 32];
            getrandom::fill(&mut bytes)?;
            if SigningKey::from_slice(&bytes).is_ok() {
                return Ok(bytes.to_vec());
            }
        }
    }
    pub fn public(reference: &[u8]) -> Result<Vec<u8>> {
        let key = SigningKey::from_slice(reference).context("invalid test key")?;
        Ok(key.verifying_key().to_encoded_point(true).as_bytes().to_vec())
    }
    pub fn sign(reference: &[u8], message: &[u8]) -> Result<Vec<u8>> {
        let key = SigningKey::from_slice(reference).context("invalid test key")?;
        let signature: p256::ecdsa::Signature = key.sign(message);
        Ok(signature.to_der().as_bytes().to_vec())
    }
}
