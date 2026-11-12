//! Owner-only files, written whole: a new temp file in the same directory,
//! `0600`, renamed over the target, so a reader never sees half a file and
//! nobody else on the machine can read it.
//!
//! The temp file's name is random and it is created with `create_new`
//! (`O_EXCL`), so it can be neither a file someone else made first nor a
//! symbolic link. The rename replaces a link at the target itself; it never
//! writes through one.

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

use pk_cli_core::CliError;

const TEMP_ATTEMPTS: usize = 8;

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
    let (tmp, mut f) = create_temp(dir, &name).map_err(|e| fail("creating a file next to", e))?;
    let result = (|| {
        f.write_all(bytes).map_err(|e| fail("writing", e))?;
        f.sync_all().map_err(|e| fail("writing", e))?;
        drop(f);
        std::fs::rename(&tmp, path).map_err(|e| fail("replacing", e))
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// A new owner-only file `.<name>.<random>.tmp` in `dir`.
fn create_temp(dir: &Path, name: &str) -> std::io::Result<(PathBuf, File)> {
    let mut last = None;
    for _ in 0..TEMP_ATTEMPTS {
        let mut suffix = [0u8; 8];
        getrandom::fill(&mut suffix).map_err(|e| std::io::Error::other(e.to_string()))?;
        let suffix: String = suffix.iter().map(|b| format!("{b:02x}")).collect();
        let tmp = dir.join(format!(".{name}.{suffix}.tmp"));
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        match opts.open(&tmp) {
            Ok(f) => return Ok((tmp, f)),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => last = Some(e),
            Err(e) => return Err(e),
        }
    }
    Err(last.unwrap_or_else(|| std::io::Error::other("no free temporary name")))
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

    #[cfg(unix)]
    #[test]
    fn a_temp_name_is_never_reused() {
        let dir = tempfile::tempdir().unwrap();
        let (a, _fa) = create_temp(dir.path(), "x").unwrap();
        let (b, _fb) = create_temp(dir.path(), "x").unwrap();
        assert_ne!(a, b);
        // A planted link cannot be opened as the temp file.
        let planted = dir.path().join(".x.planted.tmp");
        std::os::unix::fs::symlink(dir.path().join("target"), &planted).unwrap();
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        assert!(opts.open(&planted).is_err());
        assert!(!dir.path().join("target").exists());
    }
}
