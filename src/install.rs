//! Where Daisy.app runs from, and moving it to Applications.
//!
//! A downloaded app opened from Downloads or a disk image runs from a
//! read-only, randomized copy that macOS makes for quarantined apps (App
//! Translocation). Daisy offers to copy itself to Applications and relaunch
//! from there. `macos::install` gathers the facts and carries out the move.

use std::path::{Path, PathBuf};

const BUNDLE_NAME: &str = "Daisy.app";

/// Whether to offer the move, given where the bundle really lives (its
/// original path when translocated) and whether macOS considers it
/// downloaded. A bundle that was never downloaded, like a local build, is
/// left where it is.
pub fn should_offer(original: &Path, home: &Path, downloaded: bool) -> bool {
    downloaded
        && !applications_folders(home)
            .iter()
            .any(|folder| original.starts_with(folder))
}

/// The folder to copy into: the system Applications folder when this user
/// can write to it, otherwise their own.
pub fn destination(home: &Path, system_writable: bool) -> PathBuf {
    let [system, user] = applications_folders(home);
    if system_writable { system } else { user }.join(BUNDLE_NAME)
}

fn applications_folders(home: &Path) -> [PathBuf; 2] {
    [PathBuf::from("/Applications"), home.join("Applications")]
}

/// The mount point of the disk image holding `original`, from the property
/// list `hdiutil info -plist` prints. Only a disk image's volume is ever
/// ejected; an external drive is not.
pub fn image_mount_point(hdiutil_info: &[u8], original: &Path) -> Option<PathBuf> {
    let info: plist::Value = plist::from_bytes(hdiutil_info).ok()?;
    info.as_dictionary()?
        .get("images")?
        .as_array()?
        .iter()
        .filter_map(|image| image.as_dictionary()?.get("system-entities")?.as_array())
        .flatten()
        .filter_map(|entity| entity.as_dictionary()?.get("mount-point")?.as_string())
        .map(PathBuf::from)
        .filter(|mount| mount.starts_with("/Volumes") && mount != Path::new("/Volumes"))
        .find(|mount| original.starts_with(mount))
}

/// `/bin/sh` arguments that wait for process `pid` to exit, open `bundle`,
/// then eject `eject` if given. Paths travel as positional parameters, never
/// as script text.
pub fn relaunch_command(pid: u32, bundle: &Path, eject: Option<&Path>) -> Vec<String> {
    vec![
        "-c".to_owned(),
        r#"while kill -0 "$1" 2>/dev/null; do sleep 0.2; done; /usr/bin/open "$2"; [ -z "$3" ] || /usr/bin/hdiutil detach -quiet "$3""#
            .to_owned(),
        "daisy-relaunch".to_owned(),
        pid.to_string(),
        bundle.display().to_string(),
        eject.map(|path| path.display().to_string()).unwrap_or_default(),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    const HOME: &str = "/Users/someone";

    #[test]
    fn a_download_outside_applications_is_offered_the_move() {
        let home = Path::new(HOME);
        for path in [
            "/Users/someone/Downloads/Daisy.app",
            "/Volumes/Daisy/Daisy.app",
            "/Users/someone/Desktop/Daisy.app",
        ] {
            assert!(should_offer(Path::new(path), home, true), "{path}");
        }
    }

    #[test]
    fn applications_folders_and_local_builds_are_left_alone() {
        let home = Path::new(HOME);
        for path in [
            "/Applications/Daisy.app",
            "/Applications/Utilities/Daisy.app",
            "/Users/someone/Applications/Daisy.app",
        ] {
            assert!(!should_offer(Path::new(path), home, true), "{path}");
        }
        assert!(!should_offer(
            Path::new("/Users/someone/src/daisy/target/Daisy.app"),
            home,
            false
        ));
        // a folder that merely starts with the same letters is not Applications
        assert!(should_offer(Path::new("/ApplicationsOld/Daisy.app"), home, true));
    }

    #[test]
    fn copies_go_to_the_users_applications_without_write_access() {
        let home = Path::new(HOME);
        assert_eq!(destination(home, true), Path::new("/Applications/Daisy.app"));
        assert_eq!(
            destination(home, false),
            Path::new("/Users/someone/Applications/Daisy.app")
        );
    }

    const HDIUTIL_INFO: &str = r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>framework</key><string>705.0.2</string>
    <key>images</key>
    <array>
        <dict>
            <key>image-path</key><string>/Users/someone/Downloads/Daisy-0.1.3-macos-arm64.dmg</string>
            <key>system-entities</key>
            <array>
                <dict><key>dev-entry</key><string>/dev/disk6</string></dict>
                <dict>
                    <key>dev-entry</key><string>/dev/disk6s1</string>
                    <key>mount-point</key><string>/Volumes/Daisy</string>
                </dict>
            </array>
        </dict>
    </array>
</dict>
</plist>"#;

    #[test]
    fn only_a_disk_image_volume_is_ejected() {
        let info = HDIUTIL_INFO.as_bytes();
        assert_eq!(
            image_mount_point(info, Path::new("/Volumes/Daisy/Daisy.app")),
            Some(PathBuf::from("/Volumes/Daisy"))
        );
        assert_eq!(image_mount_point(info, Path::new("/Volumes/Backup/Daisy.app")), None);
        assert_eq!(image_mount_point(info, Path::new("/Volumes/Daisy 1/Daisy.app")), None);
        assert_eq!(
            image_mount_point(info, Path::new("/Users/someone/Downloads/Daisy.app")),
            None
        );
        assert_eq!(
            image_mount_point(b"not a plist", Path::new("/Volumes/Daisy/Daisy.app")),
            None
        );
    }

    #[test]
    fn relaunch_passes_paths_as_arguments() {
        let hostile = Path::new("/Volumes/x\"; rm -rf ~; \"/Daisy.app");
        let command = relaunch_command(42, hostile, Some(Path::new("/Volumes/x")));
        assert!(!command[1].contains("rm -rf"));
        assert_eq!(&command[3..], ["42", &hostile.display().to_string(), "/Volumes/x"]);
        assert_eq!(relaunch_command(42, hostile, None)[5], "");
    }

    #[test]
    fn the_relaunch_script_waits_opens_and_ejects() {
        let directory = tempfile::tempdir().unwrap();
        let log = directory.path().join("log");
        let mut command = relaunch_command(
            u32::MAX,
            Path::new("/Applications/Daisy.app"),
            Some(Path::new("/Volumes/Daisy")),
        );
        command[1] = command[1]
            .replace("/usr/bin/open", &format!("echo open >> {}; echo", log.display()))
            .replace("/usr/bin/hdiutil", &format!("echo eject >> {}; echo", log.display()));
        let status = std::process::Command::new("/bin/sh")
            .args(&command)
            .stdout(std::process::Stdio::null())
            .status()
            .unwrap();
        assert!(status.success());
        assert_eq!(std::fs::read_to_string(&log).unwrap(), "open\neject\n");
    }
}
