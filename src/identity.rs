//! This system's long-term key pair, and the public keys peers are known by.

use std::fmt;
use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::Write;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::session::NOISE_PATTERN;

pub const KEY_LEN: usize = 32;

/// A peer's long-term X25519 public key.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct PublicKey([u8; KEY_LEN]);

impl PublicKey {
    pub fn from_bytes(bytes: &[u8]) -> Option<Self> {
        bytes.try_into().ok().map(Self)
    }

    pub fn as_bytes(&self) -> &[u8; KEY_LEN] {
        &self.0
    }

    pub fn to_hex(&self) -> String {
        to_hex(&self.0)
    }

    pub fn from_hex(text: &str) -> Option<Self> {
        let bytes = from_hex(text)?;
        Self::from_bytes(&bytes)
    }

    /// Short form for display, e.g. `3f9a-12c4-8e01-77ab`.
    ///
    /// Only 64 bits, so it identifies a key to a person but is not a security
    /// check; pairing authenticates keys with a one-time code instead.
    pub fn fingerprint(&self) -> String {
        self.0[..8].chunks(2).map(to_hex).collect::<Vec<_>>().join("-")
    }
}

impl fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "PublicKey({})", self.fingerprint())
    }
}

impl fmt::Display for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.fingerprint())
    }
}

/// This system's long-term key pair.
pub struct Identity {
    private: [u8; KEY_LEN],
    public: PublicKey,
}

impl Identity {
    pub fn generate() -> Result<Self> {
        let params = NOISE_PATTERN.parse()?;
        let keypair = snow::Builder::new(params).generate_keypair()?;
        let private = keypair
            .private
            .as_slice()
            .try_into()
            .context("generated private key has the wrong length")?;
        let public = PublicKey::from_bytes(&keypair.public).context("generated public key has the wrong length")?;
        Ok(Self { private, public })
    }

    /// Load the identity at `path`, creating and saving a new one if absent.
    pub fn load_or_create(path: &Path) -> Result<Self> {
        let _lock = lock(path)?;
        if path.exists() {
            return Self::load(path);
        }
        let identity = Self::generate()?;
        identity.save(path)?;
        Ok(identity)
    }

    /// Replace the identity at `path` with a new one. Every peer paired with
    /// this system stops recognizing it and must pair again.
    pub fn rotate(path: &Path) -> Result<Self> {
        let _lock = lock(path)?;
        let identity = Self::generate()?;
        identity.save(path)?;
        Ok(identity)
    }

    fn load(path: &Path) -> Result<Self> {
        let mode = fs::metadata(path)
            .with_context(|| format!("reading {}", path.display()))?
            .permissions()
            .mode();
        if mode & 0o077 != 0 {
            bail!(
                "{} is readable by other users; restrict it with `chmod 600`",
                path.display()
            );
        }

        let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        if bytes.len() != 2 * KEY_LEN {
            bail!("{} is not a daisy identity", path.display());
        }
        let (private, public) = bytes.split_at(KEY_LEN);
        Ok(Self {
            private: private.try_into().expect("split at KEY_LEN"),
            public: PublicKey::from_bytes(public).expect("split at KEY_LEN"),
        })
    }

    fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }

        // write then rename, so a crash never leaves a truncated key behind
        let temporary = path.with_extension("tmp");
        let mut file = OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&temporary)
            .with_context(|| format!("creating {}", temporary.display()))?;
        file.write_all(&self.private)?;
        file.write_all(self.public.as_bytes())?;
        file.sync_all()?;
        fs::rename(&temporary, path).with_context(|| format!("saving {}", path.display()))?;
        Ok(())
    }

    pub fn public_key(&self) -> PublicKey {
        self.public
    }

    pub(crate) fn private_key(&self) -> &[u8] {
        &self.private
    }
}

fn lock(path: &Path) -> Result<File> {
    if let Some(parent) = path.parent() {
        DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let lock_path = path.with_extension("lock");
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&lock_path)
        .with_context(|| format!("opening {}", lock_path.display()))?;
    File::lock(&file).with_context(|| format!("locking {}", lock_path.display()))?;
    Ok(file)
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
        .map(|i| u8::from_str_radix(text.get(i..i + 2)?, 16).ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn load_or_create_persists_the_same_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested").join("identity");

        let created = Identity::load_or_create(&path).unwrap();
        let loaded = Identity::load_or_create(&path).unwrap();

        assert_eq!(created.public_key(), loaded.public_key());
        assert_eq!(created.private_key(), loaded.private_key());
    }

    #[test]
    fn identity_operations_wait_for_the_process_lock() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identity");
        Identity::load_or_create(&path).unwrap();

        let held = OpenOptions::new()
            .read(true)
            .write(true)
            .open(path.with_extension("lock"))
            .unwrap();
        File::lock(&held).unwrap();

        let (done, finished) = mpsc::channel();
        let worker_path = path.clone();
        let worker = std::thread::spawn(move || {
            done.send(Identity::load_or_create(&worker_path).map(|identity| identity.public_key()))
                .unwrap();
        });

        assert!(finished.recv_timeout(Duration::from_millis(50)).is_err());
        drop(held);
        assert_eq!(
            finished.recv_timeout(Duration::from_secs(1)).unwrap().unwrap(),
            Identity::load_or_create(&path).unwrap().public_key()
        );
        worker.join().unwrap();
    }

    #[test]
    fn rotating_replaces_the_saved_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identity");
        let before = Identity::load_or_create(&path).unwrap();

        let rotated = Identity::rotate(&path).unwrap();

        assert_ne!(rotated.public_key(), before.public_key());
        assert_eq!(
            Identity::load_or_create(&path).unwrap().public_key(),
            rotated.public_key()
        );
    }

    #[test]
    fn saved_key_is_private_to_the_user() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("daisy");
        let path = home.join("identity");
        Identity::load_or_create(&path).unwrap();

        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let home_mode = fs::metadata(&home).unwrap().permissions().mode();
        assert_eq!(home_mode & 0o777, 0o700);
    }

    #[test]
    fn refuses_key_readable_by_others() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identity");
        Identity::load_or_create(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();

        let error = Identity::load_or_create(&path).err().unwrap();
        assert!(error.to_string().contains("readable by other users"), "{error}");
    }

    #[test]
    fn refuses_corrupt_key() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("identity");
        OpenOptions::new()
            .write(true)
            .create(true)
            .truncate(true)
            .mode(0o600)
            .open(&path)
            .unwrap()
            .write_all(b"short")
            .unwrap();

        assert!(Identity::load_or_create(&path).is_err());
    }

    #[test]
    fn public_key_hex_round_trips() {
        let key = Identity::generate().unwrap().public_key();
        assert_eq!(PublicKey::from_hex(&key.to_hex()), Some(key));
        assert_eq!(PublicKey::from_hex("zz"), None);
        assert_eq!(PublicKey::from_hex("abc"), None);
        assert_eq!(PublicKey::from_hex("abcd"), None);
    }

    #[test]
    fn fingerprint_is_four_groups() {
        let key = PublicKey::from_bytes(&[0xab; KEY_LEN]).unwrap();
        assert_eq!(key.fingerprint(), "abab-abab-abab-abab");
    }
}
