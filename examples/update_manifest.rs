//! Packaging metadata is generated from the same source as the release binary.

use std::path::PathBuf;

use anyhow::{Context, Result, ensure};
use daisy::update::Manifest;

fn main() -> Result<()> {
    let args: Vec<_> = std::env::args_os().skip(1).collect();
    ensure!(args.len() == 1, "expected the release archive path");
    let path = PathBuf::from(&args[0]);
    let expected = format!("Daisy-{}-macos-arm64.zip", env!("CARGO_PKG_VERSION"));
    ensure!(
        path.file_name().is_some_and(|name| name == expected.as_str()),
        "archive version does not match this build"
    );
    let archive = std::fs::read(&path).context("reading the packaged archive")?;
    ensure!(!archive.is_empty(), "release archive is empty");
    print!("{}", toml::to_string(&Manifest::for_archive(&archive))?);
    Ok(())
}
