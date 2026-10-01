//! Moving Daisy.app to Applications and relaunching it.

use std::ffi::{CString, c_void};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use objc2::rc::Retained;
use objc2_app_kit::NSRunningApplication;
use objc2_foundation::{NSBundle, NSFileManager, NSString, NSURL};

use crate::install;

/// How long another running copy gets to quit before the move gives up.
const QUIT_WAIT: Duration = Duration::from_secs(5);

/// A move to offer.
pub struct Move {
    /// The bundle this process runs from, possibly a translocated copy.
    source: PathBuf,
    pub destination: PathBuf,
    /// A copy is already at the destination.
    pub replaces: bool,
    /// The disk image volume Daisy was opened from.
    eject: Option<PathBuf>,
}

/// The move to offer, if Daisy was downloaded and is not in Applications.
pub fn offer(home: &Path) -> Option<Move> {
    let source = running_bundle()?;
    let original = original_path(&source);
    let translocated = original.is_some();
    let original = original.unwrap_or_else(|| source.clone());
    let eject = original
        .starts_with("/Volumes")
        .then(|| {
            let info = std::process::Command::new("/usr/bin/hdiutil")
                .args(["info", "-plist"])
                .output()
                .ok()?;
            install::image_mount_point(&info.stdout, &original)
        })
        .flatten();
    // reading the original in Downloads would ask for access to that folder
    let downloaded = translocated || eject.is_some() || quarantined(&source);
    if !install::should_offer(&original, home, downloaded) {
        return None;
    }
    let destination = install::destination(home, writable(Path::new("/Applications")));
    Some(Move {
        replaces: destination.exists(),
        source,
        destination,
        eject,
    })
}

/// Copies Daisy to its destination and arranges for it to open there once
/// this process exits. The caller then quits.
pub fn carry_out(planned: &Move, bundle_id: &str) -> Result<()> {
    quit_other_copies(bundle_id)?;
    let copy = |from: &Path, to: &Path| {
        let copied = std::process::Command::new("/usr/bin/ditto")
            .arg(from)
            .arg(to)
            .status()
            .context("running ditto")?;
        if !copied.success() {
            bail!("copying to {} failed: {copied}", to.display());
        }
        Ok(())
    };
    let trash = |path: &Path| {
        let url = NSURL::fileURLWithPath(&NSString::from_str(&path.display().to_string()));
        NSFileManager::defaultManager()
            .trashItemAtURL_resultingItemURL_error(&url, None)
            .map_err(|error| anyhow::anyhow!("{}", error.localizedDescription()))
    };
    install::replace(&planned.source, &planned.destination, copy, trash)?;
    // a copy that keeps the download's quarantine would be translocated again
    let _ = std::process::Command::new("/usr/bin/xattr")
        .args(["-dr", "com.apple.quarantine"])
        .arg(&planned.destination)
        .status();
    relaunch_from(&planned.destination, planned.eject.as_deref())
}

/// Arranges for Daisy to open again from where it really lives once this
/// process exits. The caller then quits.
pub fn relaunch() -> Result<()> {
    let bundle = running_bundle().context("Daisy is not running from an app bundle")?;
    let original = original_path(&bundle).unwrap_or(bundle);
    relaunch_from(&original, None)
}

fn relaunch_from(bundle: &Path, eject: Option<&Path>) -> Result<()> {
    std::process::Command::new("/bin/sh")
        .args(install::relaunch_command(std::process::id(), bundle, eject))
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .context("starting the relaunch")?;
    Ok(())
}

fn quit_other_copies(bundle_id: &str) -> Result<()> {
    let me = std::process::id() as i32;
    let others: Vec<i32> =
        NSRunningApplication::runningApplicationsWithBundleIdentifier(&NSString::from_str(bundle_id))
            .iter()
            .filter(|app| app.processIdentifier() != me)
            .map(|app| {
                app.terminate();
                app.processIdentifier()
            })
            .collect();
    let started = Instant::now();
    while others.iter().any(|&pid| crate::launcher::process_exists(pid)) {
        if started.elapsed() > QUIT_WAIT {
            bail!("another copy of Daisy is still open; quit it and try again");
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Ok(())
}

fn running_bundle() -> Option<PathBuf> {
    let path = PathBuf::from(NSBundle::mainBundle().bundlePath().to_string());
    (path.extension()? == "app").then_some(path)
}

/// Where a translocated bundle was opened from, or `None` when `bundle` is
/// not translocated.
fn original_path(bundle: &Path) -> Option<PathBuf> {
    let url = NSURL::fileURLWithPath(&NSString::from_str(&bundle.display().to_string()));
    let url_ref = Retained::as_ptr(&url).cast::<c_void>();
    let mut translocated = false;
    // SAFETY: NSURL is toll-free bridged to CFURL; out-pointers are valid locals
    let checked = unsafe { SecTranslocateIsTranslocatedURL(url_ref, &mut translocated, std::ptr::null_mut()) };
    if !checked || !translocated {
        return None;
    }
    // SAFETY: as above; the returned URL follows the Create rule and is
    // adopted by Retained, which releases it
    let original = unsafe {
        let original = SecTranslocateCreateOriginalPathForURL(url_ref, std::ptr::null_mut());
        Retained::from_raw(original.cast_mut().cast::<NSURL>())?
    };
    Some(PathBuf::from(original.path()?.to_string()))
}

fn quarantined(path: &Path) -> bool {
    let Ok(path) = CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: both strings are NUL-terminated; a null buffer asks only for the size
    unsafe {
        getxattr(
            path.as_ptr(),
            c"com.apple.quarantine".as_ptr(),
            std::ptr::null_mut(),
            0,
            0,
            0,
        ) >= 0
    }
}

fn writable(path: &Path) -> bool {
    let Ok(path) = CString::new(path.as_os_str().as_bytes()) else {
        return false;
    };
    // SAFETY: path is NUL-terminated
    unsafe { access(path.as_ptr(), W_OK) == 0 }
}

const W_OK: i32 = 2;

#[link(name = "Security", kind = "framework")]
unsafe extern "C" {
    fn SecTranslocateIsTranslocatedURL(path: *const c_void, is_translocated: *mut bool, error: *mut c_void) -> bool;
    fn SecTranslocateCreateOriginalPathForURL(translocated_path: *const c_void, error: *mut c_void) -> *const c_void;
}

unsafe extern "C" {
    fn getxattr(
        path: *const std::ffi::c_char,
        name: *const std::ffi::c_char,
        value: *mut c_void,
        size: usize,
        position: u32,
        options: i32,
    ) -> isize;
    fn access(path: *const std::ffi::c_char, mode: i32) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_outside_translocation_has_no_original() {
        assert_eq!(original_path(Path::new("/Applications/Safari.app")), None);
    }

    #[test]
    fn quarantine_is_read_from_the_extended_attribute() {
        let directory = tempfile::tempdir().unwrap();
        let bundle = directory.path().join("Daisy.app");
        std::fs::create_dir(&bundle).unwrap();
        assert!(!quarantined(&bundle));
        let status = std::process::Command::new("/usr/bin/xattr")
            .args(["-w", "com.apple.quarantine", "0081;00000000;Safari;"])
            .arg(&bundle)
            .status()
            .unwrap();
        assert!(status.success());
        assert!(quarantined(&bundle));
    }
}
