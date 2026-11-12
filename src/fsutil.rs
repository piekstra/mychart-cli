//! Owner-only files, through the family's one writer
//! (`pk_cli_drive::private_file`): a temp file beside the target, created
//! exclusively (`O_EXCL`) with mode `0600` and renamed over it. A reader
//! never sees half a file, nobody else on the machine can read it, and a
//! file or symbolic link planted at the temp name or the target never
//! receives the bytes (a planted temp name makes the write fail instead).

use std::path::Path;

use pk_cli_core::CliError;
use pk_cli_drive::private_file::{self, Replace};

pub fn write_private(path: &Path, bytes: &[u8]) -> Result<(), CliError> {
    let dir = path
        .parent()
        .filter(|d| !d.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    create_private_dir(dir)?;
    private_file::write(path, bytes, Replace::Always)
        .map_err(|e| CliError::Other(format!("writing {}: {e}", path.display())))
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
