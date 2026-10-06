//! Fault-injected restart transactions; no real app or event tap is started.

use std::fs;
use std::path::Path;

use anyhow::{Result, bail, ensure};
use daisy::install::update::{Build, Platform, Staged, StartupGate, StartupState, restart};

fn build(version: &str) -> Build {
    Build {
        version: version.into(),
        protocol: 6,
    }
}

fn put(path: &Path, value: &Build) {
    fs::create_dir_all(path).unwrap();
    fs::write(path.join("build.toml"), toml::to_string(value).unwrap()).unwrap();
}

fn read(path: &Path) -> Build {
    toml::from_str(&fs::read_to_string(path.join("build.toml")).unwrap()).unwrap()
}

struct System {
    running: bool,
    fail: Option<&'static str>,
    second_failure: Option<&'static str>,
    events: Vec<String>,
}

impl System {
    fn new() -> Self {
        Self {
            running: true,
            fail: None,
            second_failure: None,
            events: Vec::new(),
        }
    }
    fn act(&mut self, action: &str) -> Result<()> {
        self.events.push(action.into());
        if self.fail == Some(action) {
            self.fail = self.second_failure.take();
            bail!("injected {action} failure");
        }
        Ok(())
    }
}

impl Platform for System {
    fn copy(&mut self, from: &Path, to: &Path) -> Result<()> {
        self.act(if to.ends_with("candidate.app") {
            "copy-new"
        } else {
            "copy-old"
        })?;
        put(to, &read(from));
        Ok(())
    }
    fn verify(&mut self, bundle: &Path, expected: &Build) -> Result<()> {
        self.act(if bundle.ends_with("candidate.app") {
            "verify-new"
        } else {
            "verify-old"
        })?;
        ensure!(read(bundle) == *expected, "bundle metadata mismatch");
        Ok(())
    }
    fn stop(&mut self, bundle: &Path, force: bool) -> Result<()> {
        let new = read(bundle).version == "0.7.0";
        assert_eq!(force, new, "only a failed replacement may be forced to stop");
        self.act(if new { "stop-new" } else { "stop-old" })?;
        self.running = false;
        Ok(())
    }
    fn listener_available(&mut self) -> Result<()> {
        assert!(!self.running, "the old process still owns the listener");
        self.act("port-free")
    }
    fn exchange(&mut self, a: &Path, b: &Path) -> Result<()> {
        assert!(!self.running, "replacement ran before process exit");
        self.act(if read(b).version == "0.7.0" {
            "rollback"
        } else {
            "exchange"
        })?;
        let temporary = a.with_extension("swap");
        fs::rename(a, &temporary)?;
        fs::rename(b, a)?;
        fs::rename(temporary, b)?;
        Ok(())
    }
    fn launch(&mut self, bundle: &Path, home: &Path, transaction: Option<&Path>) -> Result<()> {
        assert!(!self.running, "two processes would own the listener");
        assert_eq!(fs::read(home.join("identity")).unwrap(), b"keep device identity");
        assert_eq!(
            fs::read(home.join("peers")).unwrap(),
            b"keep paired trust and arrangement"
        );
        let new = read(bundle).version == "0.7.0";
        assert_eq!(transaction.is_some(), new);
        self.act(if new { "launch-new" } else { "launch-old" })?;
        self.running = true;
        Ok(())
    }
    fn await_ready(&mut self, bundle: &Path, root: &Path, token: &str) -> Result<()> {
        assert!(self.running);
        assert_eq!(read(bundle), build("0.7.0"));
        assert_eq!(read(&root.join("previous.app")), build("0.6.0"));
        assert_eq!(read(&root.join("candidate.app")), build("0.6.0"));
        assert_eq!(token.len(), 64);
        self.act("health")
    }
}

fn setup() -> (tempfile::TempDir, System) {
    let directory = tempfile::tempdir().unwrap();
    put(&directory.path().join("Apps/Daisy.app"), &build("0.6.0"));
    put(&directory.path().join("Download/Daisy.app"), &build("0.7.0"));
    let home = directory.path().join("data");
    fs::create_dir(&home).unwrap();
    fs::write(home.join("identity"), b"keep device identity").unwrap();
    fs::write(home.join("peers"), b"keep paired trust and arrangement").unwrap();
    (directory, System::new())
}

fn stage(directory: &Path, system: &mut System) -> Result<Staged> {
    Staged::prepare(
        &directory.join("Download/Daisy.app"),
        &directory.join("Apps/Daisy.app"),
        &directory.join("data"),
        build("0.6.0"),
        build("0.7.0"),
        system,
    )
}

#[test]
fn staging_keeps_the_old_app_and_listener_running() {
    let (directory, mut system) = setup();
    let staged = stage(directory.path(), &mut system).unwrap();
    assert!(system.running);
    assert_eq!(read(&staged.plan.target), build("0.6.0"));
    assert_eq!(system.events, ["copy-new", "verify-new", "copy-old", "verify-old"]);
    let root = staged.root.clone();
    assert!(stage(directory.path(), &mut system).is_err());
    assert!(
        root.join("previous.app").exists(),
        "another attempt removed the active transaction"
    );
    drop(staged);
    assert!(!root.exists());
    assert_eq!(read(&directory.path().join("Apps/Daisy.app")), build("0.6.0"));
}

#[test]
fn replacement_waits_for_process_exit_and_keeps_backup_until_health() {
    let (directory, mut system) = setup();
    let mut staged = stage(directory.path(), &mut system).unwrap();
    system.events.clear();
    staged.hand_off();
    restart(&staged.root, &mut system).unwrap();
    assert_eq!(
        system.events,
        [
            "verify-old",
            "verify-new",
            "verify-old",
            "stop-old",
            "port-free",
            "exchange",
            "launch-new",
            "health"
        ]
    );
    assert_eq!(read(&staged.plan.target), build("0.7.0"));
    assert!(system.running);
    assert!(!staged.root.exists());
}

#[test]
fn staging_failures_leave_the_old_installation_and_data_untouched() {
    for failure in ["copy-new", "verify-new", "copy-old", "verify-old"] {
        let (directory, mut system) = setup();
        system.fail = Some(failure);
        assert!(stage(directory.path(), &mut system).is_err(), "{failure}");
        assert!(system.running, "{failure}");
        assert_eq!(read(&directory.path().join("Apps/Daisy.app")), build("0.6.0"));
        assert!(!directory.path().join("Apps/.Daisy.app.update").exists());
    }
}

#[test]
fn failed_launch_or_health_restores_and_reopens_the_old_app() {
    for failure in ["launch-new", "health"] {
        let (directory, mut system) = setup();
        let mut staged = stage(directory.path(), &mut system).unwrap();
        staged.hand_off();
        system.events.clear();
        system.fail = Some(failure);
        let error = restart(&staged.root, &mut system).unwrap_err();
        assert!(error.to_string().contains("previous app was restored"));
        assert_eq!(read(&staged.plan.target), build("0.6.0"));
        assert!(system.running);
        assert_eq!(
            &system.events[system.events.len() - 4..],
            ["stop-new", "port-free", "rollback", "launch-old"]
        );
        assert!(!staged.root.exists());
    }
}

#[test]
fn stop_or_replacement_failure_never_discards_the_old_app() {
    for failure in ["stop-old", "port-free", "exchange"] {
        let (directory, mut system) = setup();
        let mut staged = stage(directory.path(), &mut system).unwrap();
        staged.hand_off();
        system.fail = Some(failure);
        assert!(restart(&staged.root, &mut system).is_err());
        assert_eq!(read(&staged.plan.target), build("0.6.0"));
        assert!(!staged.root.exists());
        system.fail = None;
        drop(staged);
        stage(directory.path(), &mut system).unwrap();
    }
}

#[test]
fn failed_rollback_keeps_the_previous_bundle_for_recovery() {
    for failure in ["stop-new", "rollback", "launch-old"] {
        let (directory, mut system) = setup();
        let mut staged = stage(directory.path(), &mut system).unwrap();
        staged.hand_off();
        system.fail = Some("health");
        system.second_failure = Some(failure);
        assert!(restart(&staged.root, &mut system).is_err());
        assert_eq!(read(&staged.previous()), build("0.6.0"));
        assert!(staged.plan.target.exists());
        assert!(staged.root.exists());
    }
}

#[test]
fn startup_requires_both_ui_and_controller_and_rejects_failures() {
    for backend in [StartupState::IdleReady, StartupState::SharingReady] {
        let mut gate = StartupGate::default();
        assert!(!gate.observe(StartupState::UiReady));
        assert!(gate.observe(backend));
        let mut gate = StartupGate::default();
        assert!(!gate.observe(backend));
        assert!(gate.observe(StartupState::UiReady));
        let mut gate = StartupGate::default();
        assert!(!gate.observe(StartupState::Failed));
        assert!(!gate.observe(StartupState::UiReady));
        assert!(!gate.observe(backend));
    }
}

#[test]
fn interrupted_cutover_restores_old_before_starting_another_listener() {
    let (directory, mut system) = setup();
    let mut staged = stage(directory.path(), &mut system).unwrap();
    staged.hand_off();
    system.running = false;
    system.exchange(&staged.candidate(), &staged.plan.target).unwrap();
    system.running = true;
    assert_eq!(read(&staged.plan.target), build("0.7.0"));
    let error = restart(&staged.root, &mut system).unwrap_err();
    assert!(error.to_string().contains("interrupted update was recovered"));
    assert_eq!(read(&staged.plan.target), build("0.6.0"));
    assert!(system.running);
    assert!(!staged.root.exists());
}

#[test]
fn unknown_or_breaking_build_metadata_never_stages_an_update() {
    for next in [
        Build {
            version: "0.7.0".into(),
            protocol: 7,
        },
        build("0.6.0"),
        build("0.5.0"),
        build("0.7.0-rc.1"),
        build("latest"),
    ] {
        let (directory, mut system) = setup();
        assert!(
            Staged::prepare(
                &directory.path().join("Download/Daisy.app"),
                &directory.path().join("Apps/Daisy.app"),
                &directory.path().join("data"),
                build("0.6.0"),
                next,
                &mut system
            )
            .is_err()
        );
        assert!(system.events.is_empty());
        assert!(system.running);
        assert!(!directory.path().join("Apps/.Daisy.app.update").exists());
    }
}

#[test]
fn packaging_metadata_does_not_initialize_user_data_or_sharing() {
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("data");
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_daisy"))
        .arg("--home")
        .arg(&home)
        .arg("update-info")
        .output()
        .unwrap();
    assert!(output.status.success());
    let reported: Build = toml::from_str(std::str::from_utf8(&output.stdout).unwrap()).unwrap();
    assert_eq!(reported, Build::this_system());
    assert!(!home.exists(), "the packaging command initialized a device identity");
}
