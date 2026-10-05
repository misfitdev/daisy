//! Standalone capture; does not start Daisy or touch its running session.
fn main() -> anyhow::Result<()> {
    let path = std::env::args_os()
        .nth(1)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from("daisy-swipe-report.txt"));
    daisy::macos::swipe::diagnose(&path)
}
