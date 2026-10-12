//! Private, atomic replacement of persisted application state.

use std::fs::File;
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use anyhow::{Context, Result};
use atomic_write_file::{AtomicWriteFile, unix::OpenOptionsExt as _};

/// Callers retain responsibility for parent directories and read-modify-write locks.
pub(crate) fn write(path: &Path, contents: &[u8]) -> Result<()> {
    replace(path, |file| file.write_all(contents)).with_context(|| format!("saving {}", path.display()))
}

fn replace(path: &Path, write_contents: impl FnOnce(&mut File) -> io::Result<()>) -> io::Result<()> {
    let mut file = AtomicWriteFile::options()
        .mode(0o600)
        .preserve_mode(false)
        .preserve_owner(false)
        .open(path)?;
    write_contents(file.as_file_mut())?;
    file.commit()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::{PermissionsExt, symlink};

    #[test]
    fn contents_are_private_during_writing_and_after_replacement() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("state");
        for existing in [false, true] {
            if existing {
                fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();
            }
            replace(&path, |file| {
                assert_eq!(file.metadata()?.permissions().mode() & 0o777, 0o600);
                file.write_all(b"private state")
            })
            .unwrap();
            assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
            assert_eq!(fs::read(&path).unwrap(), b"private state");
        }
    }

    #[test]
    fn failed_write_preserves_the_destination_and_cleans_up() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("state");
        for existing in [false, true] {
            if existing {
                fs::write(&path, b"original state").unwrap();
            }
            let result = replace(&path, |file| {
                file.write_all(b"partial replacement")?;
                Err(io::Error::other("interrupted write"))
            });
            assert!(result.is_err());
            if existing {
                assert_eq!(fs::read(&path).unwrap(), b"original state");
            } else {
                assert!(!path.exists());
            }
            assert_eq!(fs::read_dir(home.path()).unwrap().count(), usize::from(existing));
        }
    }

    #[test]
    fn concurrent_replacements_do_not_share_a_temporary_file() {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().join("state.toml");
        let sentinel = home.path().join("sentinel");
        fs::write(&sentinel, b"untouched").unwrap();
        symlink(&sentinel, path.with_extension("tmp")).unwrap();
        write(&path, b"original").unwrap();
        replace(&path, |first| {
            first.write_all(b"first")?;
            write(&path, b"second").unwrap();
            assert_eq!(fs::read(&path)?, b"second");
            Ok(())
        })
        .unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"first");
        assert_eq!(fs::read(&sentinel).unwrap(), b"untouched");
    }
}
