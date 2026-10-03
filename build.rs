use std::process::Command;

fn main() {
    // a worktree's .git is a file; these paths then do not exist and cargo
    // reruns this script on every build, which is only slower
    for path in [".git/HEAD", ".git/index", ".git/refs"] {
        println!("cargo:rerun-if-changed={path}");
    }
    let git = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .output()
            .ok()
            .filter(|output| output.status.success())
            .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_owned())
    };
    let commit = match git(&["rev-parse", "--short=12", "HEAD"]).filter(|hash| !hash.is_empty()) {
        Some(hash) if git(&["status", "--porcelain", "--untracked-files=no"]).is_some_and(|s| !s.is_empty()) => {
            format!("{hash}-modified")
        }
        Some(hash) => hash,
        None => "unknown".to_owned(),
    };
    println!("cargo:rustc-env=DAISY_COMMIT={commit}");
}
