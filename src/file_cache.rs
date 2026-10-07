//! Private clipboard staging. Published URLs remain valid for this boot because
//! pasteboard consumers do not acknowledge when they finish reading a file URL.

use std::fs::{self, File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, ensure};

use crate::files;

const CACHE_BYTES: u64 = 8 << 30;
const CACHE_ENTRIES: u64 = 200_000;

pub struct Cache {
    root: PathBuf,
}

pub struct Staging {
    path: PathBuf,
    published: bool,
    lease: Option<File>,
}

impl Cache {
    pub fn open(root: &Path, boot: &str) -> Result<Self> {
        ensure!(valid_boot(boot), "invalid clipboard cache session");
        private_directory(root)?;
        for entry in fs::read_dir(root)? {
            let entry = entry?;
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if let Some(previous) = name.strip_prefix("boot-")
                && previous != boot
                && valid_boot(previous)
            {
                remove(&entry.path());
            }
        }
        let root = root.join(format!("boot-{boot}"));
        private_directory(&root)?;
        let cache = Self { root };
        let _guard = cache.lock()?;
        cache.remove_abandoned()?;
        Ok(cache)
    }

    pub fn reserve(&self, content_bytes: u64, items: usize) -> Result<Staging> {
        ensure!(
            content_bytes <= files::MAX_BYTES && (1..=files::MAX_ITEMS).contains(&items),
            "invalid clipboard storage reservation"
        );
        let _guard = self.lock()?;
        let used = self.used(None)?;
        let entries = files::MAX_ENTRIES as u64 + items as u64 + 4;
        let bytes = content_bytes
            .checked_add(files::MAX_ATTRIBUTE_BYTES * items as u64)
            .and_then(|n| n.checked_add(entries * 4096))
            .context("clipboard cache size overflow")?;
        ensure!(
            used.0.checked_add(bytes).is_some_and(|total| total <= CACHE_BYTES)
                && used.1.checked_add(entries).is_some_and(|total| total <= CACHE_ENTRIES),
            "clipboard file cache is full; restart this system to clear cached files"
        );
        let mut random = [0; 16];
        getrandom::fill(&mut random)?;
        let path = self.root.join(format!("offer-{:032x}", u128::from_ne_bytes(random)));
        private_directory(&path)?;
        if let Err(error) = fs::write(path.join(".budget"), format!("{bytes} {entries}")) {
            remove(&path);
            return Err(error.into());
        }
        let lease = match OpenOptions::new()
            .read(true)
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(path.join(".lease"))
        {
            Ok(lease) => lease,
            Err(error) => {
                remove(&path);
                return Err(error.into());
            }
        };
        // SAFETY: this staging owns the open lease descriptor until publication
        // or cleanup. Other processes only reclaim unlocked unpublished staging.
        if unsafe { libc::flock(lease.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            remove(&path);
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(Staging {
            path,
            published: false,
            lease: Some(lease),
        })
    }

    pub fn publish(&self, mut staging: Staging) -> Result<PathBuf> {
        let _guard = self.lock()?;
        let (bytes, entries) = usage(&staging.path)?;
        let (bytes, entries) = (bytes.checked_add(512).context("cache size overflow")?, entries + 1);
        ensure!(
            bytes
                <= files::MAX_BYTES
                    + files::MAX_ATTRIBUTE_BYTES * files::MAX_ITEMS as u64
                    + files::MAX_ENTRIES as u64 * 4096
                && entries <= files::MAX_ENTRIES as u64 + files::MAX_ITEMS as u64 + 4,
            "received files exceed clipboard cache budget"
        );
        let used = self.used(Some(&staging.path))?;
        ensure!(
            used.0.checked_add(bytes).is_some_and(|n| n <= CACHE_BYTES)
                && used.1.checked_add(entries).is_some_and(|n| n <= CACHE_ENTRIES),
            "clipboard file cache is full; restart this system to clear cached files"
        );
        fs::write(staging.path.join(".budget"), format!("{bytes} {entries}"))?;
        File::create(staging.path.join(".published"))?.sync_all()?;
        File::open(&staging.path)?.sync_all()?;
        staging.published = true;
        Ok(staging.path.clone())
    }

    fn remove_abandoned(&self) -> Result<()> {
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            if !entry.file_name().to_string_lossy().starts_with("offer-") || entry.path().join(".published").exists() {
                continue;
            }
            let lease = match OpenOptions::new()
                .read(true)
                .write(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(entry.path().join(".lease"))
            {
                Ok(lease) => lease,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    remove(&entry.path());
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            // SAFETY: owned descriptor is held while reclaiming this staging.
            if unsafe { libc::flock(lease.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                remove(&entry.path());
            } else {
                let error = std::io::Error::last_os_error();
                ensure!(
                    error.kind() == std::io::ErrorKind::WouldBlock,
                    "could not check clipboard staging: {error}"
                );
            }
        }
        Ok(())
    }

    fn used(&self, except: Option<&Path>) -> Result<(u64, u64)> {
        // Account for the boot directory and its lock as well as offer staging.
        let mut used = (entry_usage(&self.root)? + entry_usage(&self.root.join(".lock"))?, 2u64);
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            if entry.file_name().to_string_lossy().starts_with("offer-") && except != Some(entry.path().as_path()) {
                let budget = fs::read_to_string(entry.path().join(".budget"))?;
                let (bytes, entries) = budget.split_once(' ').context("invalid clipboard cache budget")?;
                used.0 = used.0.checked_add(bytes.parse()?).context("cache size overflow")?;
                used.1 = used.1.checked_add(entries.parse()?).context("cache entry overflow")?;
            }
        }
        Ok(used)
    }

    fn lock(&self) -> Result<Lock> {
        lock_directory(&self.root)
    }
}

fn lock_directory(root: &Path) -> Result<Lock> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(root.join(".lock"))?;
    // SAFETY: the owned descriptor stays open throughout the guard's lifetime.
    ensure!(
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX) } == 0,
        "could not reserve clipboard storage"
    );
    Ok(Lock(file))
}

impl Staging {
    pub fn path(&self) -> &Path {
        &self.path
    }
}
impl Drop for Staging {
    fn drop(&mut self) {
        if !self.published
            && let Some(parent) = self.path.parent()
            && let Ok(_guard) = lock_directory(parent)
        {
            remove(&self.path);
        }
        self.lease.take();
    }
}
struct Lock(File);
impl Drop for Lock {
    fn drop(&mut self) {
        // SAFETY: the guard owns this descriptor.
        unsafe {
            libc::flock(self.0.as_raw_fd(), libc::LOCK_UN);
        }
    }
}
fn valid_boot(boot: &str) -> bool {
    boot.split_once('-').is_some_and(|(seconds, micros)| {
        !seconds.is_empty()
            && !micros.is_empty()
            && seconds.bytes().all(|b| b.is_ascii_digit())
            && micros.bytes().all(|b| b.is_ascii_digit())
    })
}
fn private_directory(path: &Path) -> Result<()> {
    match fs::create_dir(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error.into()),
    }
    let meta = fs::symlink_metadata(path)?;
    // SAFETY: geteuid has no preconditions or side effects.
    ensure!(
        meta.is_dir() && meta.uid() == unsafe { libc::geteuid() },
        "clipboard cache is not a private folder"
    );
    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}
fn entry_usage(path: &Path) -> Result<u64> {
    let meta = fs::symlink_metadata(path)?;
    Ok(meta.len().max(meta.blocks() * 512) + 512)
}
fn usage(path: &Path) -> Result<(u64, u64)> {
    let meta = fs::symlink_metadata(path)?;
    let mut total = (entry_usage(path)?, 1);
    if meta.is_dir() {
        for child in fs::read_dir(path)? {
            let child = usage(&child?.path())?;
            total.0 = total.0.checked_add(child.0).context("cache size overflow")?;
            total.1 += child.1;
            ensure!(
                total.1 <= files::MAX_ENTRIES as u64 + files::MAX_ITEMS as u64 + 4,
                "too many cached file entries"
            );
        }
    }
    Ok(total)
}
fn remove(path: &Path) {
    if fs::symlink_metadata(path).is_ok_and(|m| m.is_dir()) {
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
        if let Ok(children) = fs::read_dir(path) {
            for child in children.flatten() {
                remove(&child.path());
            }
        }
        let _ = fs::remove_dir(path);
    } else {
        let _ = fs::remove_file(path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn published_files_survive_clipboard_release_and_are_cleaned_only_after_reboot() {
        let root = tempfile::tempdir().unwrap();
        let cache = Cache::open(root.path(), "100-1").unwrap();
        let staging = cache.reserve(4, 1).unwrap();
        fs::write(staging.path().join("copied"), b"data").unwrap();
        let path = cache.publish(staging).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o500)).unwrap();
        drop(cache);
        Cache::open(root.path(), "100-1").unwrap();
        assert_eq!(fs::read(path.join("copied")).unwrap(), b"data");
        Cache::open(root.path(), "200-2").unwrap();
        assert!(!path.exists());
    }
    #[test]
    fn failed_materialization_releases_storage_and_reservations_are_bounded() {
        let root = tempfile::tempdir().unwrap();
        let cache = Cache::open(root.path(), "100-1").unwrap();
        let first = cache.reserve(files::MAX_BYTES, 1).unwrap();
        assert!(cache.reserve(files::MAX_BYTES, 1).is_err());
        let path = first.path().to_owned();
        drop(first);
        assert!(!path.exists());
        assert!(cache.reserve(files::MAX_BYTES, 1).is_ok());
        assert!(Cache::open(root.path(), "../outside").is_err());
    }
    #[test]
    fn publication_rechecks_total_actual_storage_and_cleans_rejected_staging() {
        let root = tempfile::tempdir().unwrap();
        let cache = Cache::open(root.path(), "100-1").unwrap();
        let first = cache.reserve(1, 1).unwrap();
        let first = cache.publish(first).unwrap();
        let second = cache.reserve(1, 1).unwrap();
        fs::write(second.path().join("copied"), b"data").unwrap();
        let actual = usage(second.path()).unwrap().0;
        fs::write(
            first.join(".budget"),
            format!(
                "{} 1",
                CACHE_BYTES
                    - entry_usage(&cache.root).unwrap()
                    - entry_usage(&cache.root.join(".lock")).unwrap()
                    - actual
                    + 1
            ),
        )
        .unwrap();
        let path = second.path().to_owned();
        assert!(cache.publish(second).is_err());
        assert!(!path.exists());
    }

    #[test]
    fn reservations_charge_item_wrappers_and_offer_bookkeeping() {
        let root = tempfile::tempdir().unwrap();
        let cache = Cache::open(root.path(), "100-1").unwrap();
        let first = cache.reserve(1, 1).unwrap();
        fs::write(
            first.path().join(".budget"),
            format!("1 {}", CACHE_ENTRIES - 2 - files::MAX_ENTRIES as u64 - 2),
        )
        .unwrap();
        assert!(cache.reserve(1, 1).is_err());
    }
    #[test]
    fn interrupted_reservations_are_reclaimed_without_removing_live_staging() {
        let root = tempfile::tempdir().unwrap();
        let cache = Cache::open(root.path(), "100-1").unwrap();
        let mut abandoned = cache.reserve(1, 1).unwrap();
        let path = abandoned.path().to_owned();
        // Model process exit: the descriptor is closed without Staging::drop.
        drop(abandoned.lease.take());
        std::mem::forget(abandoned);
        let cache = Cache::open(root.path(), "100-1").unwrap();
        assert!(!path.exists());
        let live = cache.reserve(1, 1).unwrap();
        Cache::open(root.path(), "100-1").unwrap();
        assert!(live.path().exists(), "another process is still receiving here");
    }
}
