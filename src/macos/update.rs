//! Platform execution for a staged update. No input capture or listener starts
//! in the helper; it waits for process exit before replacing or reopening.

use std::ffi::CString;
use std::fs::{self, File, OpenOptions};
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use objc2_app_kit::NSRunningApplication;
use objc2_foundation::{NSString, NSURL};

use crate::install::update::{self, Build, Platform, Ready, Staged};

pub const STARTUP_DIRECTORY: &str = "DAISY_UPDATE_DIRECTORY";
const BUNDLE_ID: &str = "dev.misfit.daisy";
const STOP_TIMEOUT: Duration = Duration::from_secs(10);
const START_TIMEOUT: Duration = Duration::from_secs(30);

pub fn running_bundle() -> Result<PathBuf> {
    crate::launcher::app_bundle(&std::env::current_exe()?).context("run the updater from the installed Daisy.app")
}

/// Run the verified release installer in a separate process so policy callers
/// do not exclude the running GUI from the restart helper's stop operation.
pub fn request_release(home: &Path, version: Option<&semver::Version>) -> Result<Child> {
    let executable = running_bundle()?.join("Contents/MacOS/daisy");
    let mut command = Command::new(executable);
    command.arg("--home").arg(home).arg("update");
    if let Some(version) = version {
        command.arg("--version").arg(version.to_string());
    }
    command
        .env_remove("DAISY_LAUNCHER_PID")
        .env_remove(STARTUP_DIRECTORY)
        .spawn()
        .context("starting verified release installation")
}

/// CLI worker: downloaded data is never executed before provenance, build
/// metadata, notarization and same-publisher signature checks have passed.
pub fn install_release(home: &Path, version: Option<&semver::Version>) -> Result<Option<Child>> {
    let target = running_bundle()?;
    let mut platform = Executor::new(&target)?;
    let previous = build_info(&target)?;
    let release = match crate::install::release::fetch(&mut crate::update::ReleaseSource, &previous, version) {
        Ok(release) => release,
        Err(error) if error.is::<crate::install::release::AlreadyCurrent>() => return Ok(None),
        Err(error) => return Err(error),
    };
    let directory = tempfile::Builder::new()
        .prefix("daisy-release-")
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir()?;
    let source = release.extract(directory.path())?;
    platform.verify(&source, release.build())?;
    install_candidate(&source, home, Some(release.build())).map(Some)
}

/// Stages a locally supplied, publisher-signed update. The release installer
/// must check download provenance before using this same handoff API.
pub fn install_local(source: &Path, home: &Path) -> Result<Child> {
    install_candidate(source, home, None)
}

fn install_candidate(source: &Path, home: &Path, expected: Option<&Build>) -> Result<Child> {
    let target = running_bundle()?;
    let mut platform = Executor::new(&target)?;
    platform.verify_signature(source)?;
    let next = build_info(source)?;
    ensure!(
        expected.is_none_or(|expected| expected == &next),
        "the app no longer matches the verified release metadata"
    );
    let mut staged = Staged::prepare(source, &target, home, Build::this_system(), next, &mut platform)?;
    let child = Command::new(staged.helper())
        .arg("apply-update")
        .arg(&staged.root)
        .arg("--exclude-process")
        .arg(std::process::id().to_string())
        .env_remove("DAISY_LAUNCHER_PID")
        .env_remove(STARTUP_DIRECTORY)
        .spawn()
        .context("starting the update helper")?;
    staged.hand_off();
    Ok(child)
}

pub fn apply(root: &Path, exclude_process: u32) -> Result<()> {
    let plan = update::read_plan(root)?;
    let _lock = lock(root)?;
    let mut platform = Executor::new(&root.join("previous.app"))?;
    platform.excluded_process = exclude_process;
    // The retained helper is signed by the same publisher as the installed app.
    platform.verify(&root.join("previous.app"), &plan.previous)?;
    update::restart(root, &mut platform)
}

/// If a replacement was interrupted before its startup acknowledgment, the
/// next ordinary launch delegates recovery to the retained old binary and
/// exits before binding or capturing input. Normal helper launches carry the
/// transaction environment and follow the acknowledgment path instead.
pub fn recover_pending() -> Result<bool> {
    if std::env::var_os(STARTUP_DIRECTORY).is_some_and(|value| !value.is_empty()) {
        return Ok(false);
    }
    let Ok(target) = running_bundle() else {
        return Ok(false);
    };
    let Some(parent) = target.parent() else {
        return Ok(false);
    };
    let root = parent.join(".Daisy.app.update");
    let Ok(plan) = update::read_plan(&root) else {
        return Ok(false);
    };
    if plan.next != Build::this_system() {
        return Ok(false);
    }
    let _lock = match lock(&root) {
        Ok(lock) => lock,
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::WouldBlock) =>
        {
            return Ok(true);
        }
        Err(error) => return Err(error),
    };
    let mut executor = Executor::new(&target)?;
    executor.verify(&target, &plan.next)?;
    if root.join("committed").exists() {
        fs::remove_dir_all(&root).context("removing the committed update backup")?;
        return Ok(false);
    }
    executor.verify(&root.join("previous.app"), &plan.previous)?;
    // Release the lock before the helper takes over. This bootstrap process
    // returns immediately, leaving no old listener for the helper to compete
    // with while it recovers the installation.
    drop(_lock);
    Command::new(root.join("previous.app/Contents/MacOS/daisy"))
        .arg("apply-update")
        .arg(&root)
        .env_remove("DAISY_LAUNCHER_PID")
        .env_remove(STARTUP_DIRECTORY)
        .spawn()
        .context("starting interrupted-update recovery")?;
    Ok(true)
}

struct Executor {
    requirement: String,
    excluded_process: u32,
}

impl Executor {
    fn new(trusted: &Path) -> Result<Self> {
        let output = Command::new("/usr/bin/codesign")
            .args(["-d", "--verbose=4"])
            .arg(trusted)
            .output()?;
        ensure!(
            output.status.success(),
            "the installed app has no valid signing identity"
        );
        let details = String::from_utf8_lossy(&output.stderr);
        let team = details
            .lines()
            .find_map(|line| line.strip_prefix("TeamIdentifier="))
            .context("the installed app has no publisher team")?;
        ensure!(
            team.len() == 10 && team.bytes().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit()),
            "the installed app needs a Developer ID signature"
        );
        let requirement = format!(
            "identifier \"{BUNDLE_ID}\" and anchor apple generic and certificate leaf[subject.OU] = \"{team}\" and certificate leaf[field.1.2.840.113635.100.6.1.13] exists"
        );
        let executor = Self {
            requirement,
            excluded_process: 0,
        };
        executor.verify_signature(trusted)?;
        Ok(executor)
    }

    fn verify_signature(&self, bundle: &Path) -> Result<()> {
        let output = Command::new("/usr/bin/codesign")
            .args(["--verify", "--deep", "--strict", "--all-architectures"])
            .arg(format!("-R={}", self.requirement))
            .arg(bundle)
            .output()?;
        ensure!(
            output.status.success(),
            "the app does not have Daisy's valid publisher signature: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        let assessed = Command::new("/usr/sbin/spctl")
            .args(["--assess", "--type", "execute", "--verbose=4"])
            .arg(bundle)
            .output()?;
        ensure!(
            assessed.status.success()
                && String::from_utf8_lossy(&assessed.stderr)
                    .lines()
                    .any(|line| line == "source=Notarized Developer ID"),
            "the app is not a notarized Developer ID release: {}",
            String::from_utf8_lossy(&assessed.stderr)
        );
        Ok(())
    }
}

impl Platform for Executor {
    fn copy(&mut self, from: &Path, to: &Path) -> Result<()> {
        ensure!(
            Command::new("/usr/bin/ditto").arg(from).arg(to).status()?.success(),
            "the app could not be staged"
        );
        Ok(())
    }

    fn verify(&mut self, bundle: &Path, build: &Build) -> Result<()> {
        self.verify_signature(bundle)?;
        ensure!(
            build_info(bundle)? == *build,
            "the app's signed version or protocol does not match this update"
        );
        Ok(())
    }

    fn stop(&mut self, bundle: &Path, force: bool) -> Result<()> {
        let apps = applications(bundle, self.excluded_process);
        let pids: Vec<_> = apps.iter().map(|app| app.processIdentifier()).collect();
        for app in &apps {
            app.terminate();
        }
        let mut started = Instant::now();
        let mut forced = false;
        while pids.iter().any(|&pid| crate::launcher::process_exists(pid)) {
            if force && !forced && started.elapsed() >= STOP_TIMEOUT {
                for app in &apps {
                    app.forceTerminate();
                }
                started = Instant::now();
                forced = true;
            }
            ensure!(
                started.elapsed() < STOP_TIMEOUT,
                "Daisy has not exited; its bundle was retained"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
        Ok(())
    }

    fn listener_available(&mut self) -> Result<()> {
        listener_available_on(std::net::SocketAddr::from(([0, 0, 0, 0], crate::service::DEFAULT_PORT)))
            .context("port 24850 has not been released")
    }

    fn exchange(&mut self, a: &Path, b: &Path) -> Result<()> {
        let a = CString::new(a.as_os_str().as_bytes())?;
        let b = CString::new(b.as_os_str().as_bytes())?;
        // SAFETY: both paths are NUL terminated. RENAME_SWAP exchanges two
        // directories atomically on the same filesystem; neither is removed.
        let status = unsafe { renamex_np(a.as_ptr(), b.as_ptr(), 0x0000_0002) };
        ensure!(
            status == 0,
            "atomic app exchange failed: {}",
            std::io::Error::last_os_error()
        );
        Ok(())
    }

    fn launch(&mut self, bundle: &Path, home: &Path, transaction: Option<&Path>) -> Result<()> {
        let mut open = Command::new("/usr/bin/open");
        open.args(["-n", "-g", "--env"]);
        let mut environment = std::ffi::OsString::from(format!("{STARTUP_DIRECTORY}="));
        if let Some(root) = transaction {
            environment.push(root);
        }
        open.arg(environment).arg(bundle).arg("--args").arg("--home").arg(home);
        ensure!(open.status()?.success(), "Daisy could not be reopened");
        Ok(())
    }

    fn await_ready(&mut self, bundle: &Path, root: &Path, token: &str) -> Result<()> {
        let started = Instant::now();
        while started.elapsed() < START_TIMEOUT {
            if let Ok(text) = fs::read_to_string(root.join("ready.toml"))
                && let Ok(ready) = toml::from_str::<Ready>(&text)
                && ready.token == token
                && applications(bundle, 0).iter().any(|app| {
                    app.processIdentifier() as u32 == ready.pid
                        && crate::launcher::process_exists(app.processIdentifier())
                })
            {
                return Ok(());
            }
            std::thread::sleep(Duration::from_millis(100));
        }
        bail!("Daisy did not acknowledge successful startup");
    }
}

fn build_info(bundle: &Path) -> Result<Build> {
    // Read the publisher-sealed plist; never execute the candidate while the
    // old app owns the listener. Packaging embeds the compiled protocol here.
    use std::io::Read;
    let mut bytes = Vec::new();
    File::open(bundle.join("Contents/Info.plist"))?
        .take(65_537)
        .read_to_end(&mut bytes)?;
    ensure!(bytes.len() <= 65_536, "update metadata is too large");
    let value = plist::Value::from_reader(std::io::Cursor::new(bytes))?;
    let fields = value.as_dictionary().context("the app has no bundle metadata")?;
    let text = |key| fields.get(key).and_then(plist::Value::as_string);
    ensure!(
        text("CFBundleIdentifier") == Some(BUNDLE_ID)
            && text("CFBundleExecutable") == Some("daisy")
            && text("CFBundlePackageType") == Some("APPL"),
        "this is not the Daisy app bundle"
    );
    let protocol = fields
        .get("DaisyNetworkProtocol")
        .and_then(plist::Value::as_unsigned_integer)
        .and_then(|value| u16::try_from(value).ok())
        .context("the app does not declare its network compatibility")?;
    Ok(Build {
        version: text("CFBundleShortVersionString")
            .context("the app has no release version")?
            .into(),
        protocol,
    })
}

fn listener_available_on(address: std::net::SocketAddr) -> Result<()> {
    // Match the service's SO_REUSEADDR behavior. A plain std listener can
    // reject a released port while the previous sessions remain in TIME_WAIT.
    // The helper has no application runtime or existing input/network tasks.
    let runtime = tokio::runtime::Builder::new_current_thread().enable_io().build()?;
    let listener = runtime.block_on(tokio::net::TcpListener::bind(address))?;
    drop(listener);
    Ok(())
}

fn applications(bundle: &Path, caller: u32) -> Vec<objc2::rc::Retained<NSRunningApplication>> {
    NSRunningApplication::runningApplicationsWithBundleIdentifier(&NSString::from_str(BUNDLE_ID))
        .iter()
        .filter(|app| {
            let pid = app.processIdentifier() as u32;
            app.processIdentifier() > 0
                && pid != caller
                && pid != std::process::id()
                && app
                    .bundleURL()
                    .and_then(|url: objc2::rc::Retained<NSURL>| url.path())
                    .is_some_and(|path| Path::new(&path.to_string()) == bundle)
        })
        .collect()
}

fn lock(root: &Path) -> Result<File> {
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(root.join("worker.lock"))?;
    // SAFETY: descriptor belongs to this File and remains open until the
    // transaction ends. LOCK_EX | LOCK_NB prevents two helpers racing.
    if unsafe { flock(lock.as_raw_fd(), 2 | 4) } != 0 {
        return Err(std::io::Error::last_os_error()).context("another update helper is already running");
    }
    Ok(lock)
}

unsafe extern "C" {
    fn renamex_np(from: *const std::ffi::c_char, to: *const std::ffi::c_char, flags: u32) -> i32;
    fn flock(fd: i32, operation: i32) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn port_probe_rejects_a_live_listener_but_accepts_closed_sessions() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()
            .unwrap();
        let (listener, address, server, client) = runtime.block_on(async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let client = tokio::net::TcpStream::connect(address).await.unwrap();
            let (server, _) = listener.accept().await.unwrap();
            (listener, address, server, client)
        });
        assert!(listener_available_on(address).is_err());
        runtime.block_on(async {
            use tokio::io::{AsyncReadExt, AsyncWriteExt};
            let mut server = server;
            let mut client = client;
            server.shutdown().await.unwrap();
            let mut byte = [0];
            assert_eq!(client.read(&mut byte).await.unwrap(), 0);
            client.shutdown().await.unwrap();
            assert_eq!(server.read(&mut byte).await.unwrap(), 0);
        });
        drop(listener);
        assert!(listener_available_on(address).is_ok());
    }

    #[test]
    fn bundle_exchange_and_failed_exchange_keep_the_installed_path_complete() {
        let directory = tempfile::tempdir().unwrap();
        let old = directory.path().join("Daisy.app");
        let new = directory.path().join("candidate.app");
        for (path, marker) in [(&old, b"old"), (&new, b"new")] {
            fs::create_dir(path).unwrap();
            fs::write(path.join("marker"), marker).unwrap();
        }
        let mut executor = Executor {
            requirement: String::new(),
            excluded_process: 0,
        };
        executor.exchange(&new, &old).unwrap();
        assert_eq!(fs::read(old.join("marker")).unwrap(), b"new");
        assert_eq!(fs::read(new.join("marker")).unwrap(), b"old");
        assert!(executor.exchange(&directory.path().join("missing"), &old).is_err());
        assert_eq!(fs::read(old.join("marker")).unwrap(), b"new");
        executor.exchange(&new, &old).unwrap();
        assert_eq!(fs::read(old.join("marker")).unwrap(), b"old");
    }

    #[test]
    fn only_one_helper_can_own_a_pending_transaction() {
        let directory = tempfile::tempdir().unwrap();
        let first = lock(directory.path()).unwrap();
        let error = lock(directory.path()).unwrap_err();
        assert_eq!(
            error.downcast_ref::<std::io::Error>().unwrap().kind(),
            std::io::ErrorKind::WouldBlock
        );
        drop(first);
        assert!(lock(directory.path()).is_ok());
    }
}
