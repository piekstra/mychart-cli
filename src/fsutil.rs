//! Owner-only files, written whole: a temp file beside the target, created
//! exclusively (`O_EXCL`) with mode `0600` by the family's writer
//! (`pk_cli_drive::private_file`), synced to disk, then renamed over the
//! target. A reader never sees half a file, even after a crash; nobody else
//! on the machine can read it; a file or symbolic link planted at the temp
//! name or the target never receives the bytes (a planted temp name makes
//! the write fail instead).
//!
//! The sync is here because `private_file::write` (cli-common v0.29.0)
//! renames without one; once a pinned cli-common syncs, this is
//! `private_file::write(path, bytes, Replace::Always)`.

use std::path::Path;

use pk_cli_core::CliError;
use pk_cli_drive::private_file::{self, Replace};

pub fn write_private(path: &Path, bytes: &[u8]) -> Result<(), CliError> {
    let fail =
        |what: &str, e: std::io::Error| CliError::Other(format!("{what} {}: {e}", path.display()));
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    create_private_dir(dir)?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".into());
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = dir.join(format!(".{name}.{}.{nanos}.tmp", std::process::id()));
    private_file::write(&tmp, bytes, Replace::Never).map_err(|e| fail("writing", e))?;
    let result = std::fs::File::open(&tmp)
        .and_then(|f| f.sync_all())
        .map_err(|e| fail("writing", e))
        .and_then(|()| std::fs::rename(&tmp, path).map_err(|e| fail("replacing", e)));
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Create `dir` (and parents) owner-only.
pub fn create_private_dir(dir: &Path) -> Result<(), CliError> {
    let mut b = std::fs::DirBuilder::new();
    b.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(0o700);
    }
    b.create(dir)
        .map_err(|e| CliError::Other(format!("creating {}: {e}", dir.display())))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_file_is_written_whole_and_owner_only_with_no_temp_left() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("records.ndjson");
        write_private(&path, b"one").unwrap();
        write_private(&path, b"two").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"two");
        let names: Vec<_> = std::fs::read_dir(path.parent().unwrap())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names, ["records.ndjson"]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = |p: &Path| std::fs::metadata(p).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode(&path), 0o600);
            assert_eq!(mode(path.parent().unwrap()), 0o700);
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_symbolic_link_at_the_target_is_replaced_not_written_through() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let elsewhere = dir.path().join("elsewhere.txt");
        std::fs::write(&elsewhere, b"untouched").unwrap();
        std::fs::set_permissions(&elsewhere, std::fs::Permissions::from_mode(0o644)).unwrap();
        let path = dir.path().join("note.html");
        std::os::unix::fs::symlink(&elsewhere, &path).unwrap();
        write_private(&path, b"<html></html>").unwrap();
        assert_eq!(std::fs::read(&elsewhere).unwrap(), b"untouched");
        let meta = std::fs::symlink_metadata(&path).unwrap();
        assert!(meta.file_type().is_file());
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    }
}
