//! Packaging metadata is available without identity or sharing initialization.
use daisy::update::Build;
use std::fs;

#[test]
fn metadata_does_not_open_a_device_identity_or_start_sharing() {
    let home = tempfile::tempdir().unwrap();
    let output = std::process::Command::new(env!("CARGO_BIN_EXE_daisy"))
        .args(["--home", home.path().to_str().unwrap(), "update-info"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let build: Build = toml::from_str(std::str::from_utf8(&output.stdout).unwrap()).unwrap();
    assert_eq!(build, Build::this_system());
    assert_eq!(fs::read_dir(home.path()).unwrap().count(), 0);
}
