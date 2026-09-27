//! Running Daisy.app from a terminal.
//!
//! macOS judges privacy permissions by the app that launched a process, so
//! the binary run straight from a terminal would borrow the terminal's.
//! Instead it relaunches itself through `open`, which makes the app
//! responsible for itself, and waits.
//!
//! `open` asks launchd to start the app, so Ctrl-C in the terminal stops the
//! launcher but not the app. The app therefore watches the launcher and
//! shuts down when it goes.

use std::ffi::{CStr, c_char, c_int};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};

/// Set on the relaunched app: the process ID of the launcher it serves.
const LAUNCHER_PID: &str = "DAISY_LAUNCHER_PID";
const WATCH_INTERVAL: Duration = Duration::from_millis(250);

/// If this is the binary inside Daisy.app run from a terminal, relaunch
/// it as the app and return its exit code. Otherwise return `None` and carry
/// on in this process.
pub fn relaunch_as_app() -> Result<Option<i32>> {
    if std::env::var_os(LAUNCHER_PID).is_some() {
        return Ok(None);
    }
    let Some(bundle) = app_bundle(&std::env::current_exe()?) else {
        return Ok(None);
    };
    let Some(terminal) = terminal() else {
        return Ok(None);
    };

    let mut open = std::process::Command::new("/usr/bin/open");
    open.args(["-n", "-W", "--env"])
        .arg(format!("{LAUNCHER_PID}={}", std::process::id()));
    // open drops this process's environment, so pass logging on explicitly
    if let Some(filter) = std::env::var_os("RUST_LOG") {
        let mut setting = std::ffi::OsString::from("RUST_LOG=");
        setting.push(filter);
        open.arg("--env").arg(setting);
    }
    for stream in ["--stdin", "--stdout", "--stderr"] {
        open.arg(stream).arg(&terminal);
    }
    open.arg(&bundle).arg("--args").args(std::env::args_os().skip(1));

    let status = open.status().context("launching Daisy.app")?;
    Ok(Some(status.code().unwrap_or(1)))
}

/// Resolves once the launcher that started this app has exited, or never if
/// this app was not started by one.
pub async fn launcher_gone() {
    let Some(pid) = std::env::var(LAUNCHER_PID)
        .ok()
        .and_then(|pid| pid.parse::<c_int>().ok())
    else {
        return std::future::pending().await;
    };
    loop {
        tokio::time::sleep(WATCH_INTERVAL).await;
        if !process_exists(pid) {
            return;
        }
    }
}

/// The .app bundle containing `executable`, if it sits at
/// `Something.app/Contents/MacOS/<name>`.
fn app_bundle(executable: &Path) -> Option<PathBuf> {
    let macos = executable.parent()?;
    let contents = macos.parent()?;
    let bundle = contents.parent()?;
    let is_bundle =
        macos.file_name()? == "MacOS" && contents.file_name()? == "Contents" && bundle.extension()? == "app";
    is_bundle.then(|| bundle.to_owned())
}

/// The terminal device standard input is connected to, if any.
fn terminal() -> Option<PathBuf> {
    // SAFETY: ttyname returns null or a NUL-terminated string that stays
    // valid until the next call, and it is copied out immediately
    unsafe {
        let name = ttyname(0);
        if name.is_null() {
            return None;
        }
        Some(PathBuf::from(CStr::from_ptr(name).to_string_lossy().into_owned()))
    }
}

fn process_exists(pid: c_int) -> bool {
    // SAFETY: signal 0 checks for the process without sending anything
    let result = unsafe { kill(pid, 0) };
    // EPERM also means it exists; only ESRCH means it is gone
    result == 0 || std::io::Error::last_os_error().raw_os_error() != Some(ESRCH)
}

const ESRCH: c_int = 3;

unsafe extern "C" {
    fn ttyname(fd: c_int) -> *const c_char;
    fn kill(pid: c_int, signal: c_int) -> c_int;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_bundle_around_its_executable() {
        assert_eq!(
            app_bundle(Path::new("/Applications/Daisy.app/Contents/MacOS/daisy")),
            Some(PathBuf::from("/Applications/Daisy.app"))
        );
        assert_eq!(app_bundle(Path::new("/tmp/daisy/target/debug/daisy")), None);
        assert_eq!(app_bundle(Path::new("/Applications/Daisy/Contents/MacOS/daisy")), None);
        assert_eq!(app_bundle(Path::new("daisy")), None);
    }

    #[test]
    fn this_process_exists_and_a_finished_one_does_not() {
        assert!(process_exists(std::process::id() as c_int));
        let mut child = std::process::Command::new("/usr/bin/true").spawn().unwrap();
        let pid = child.id() as c_int;
        child.wait().unwrap();
        assert!(!process_exists(pid));
    }
}
