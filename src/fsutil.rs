//! Owner-only files, written whole: a temp file beside the target, created
//! exclusively (`O_EXCL`) with mode `0600` by the family's writer
//! (`pk_cli_drive::private_file`), synced to disk, then renamed over the
//! target. A reader never sees half a file, even after a crash; nobody else
//! on the machine can read it; a file or symbolic link planted at the temp
//! name or the target never receives the bytes (a planted temp name makes
//! the write fail instead).
//!
//! The sync is here because `private_file::write` (cli-common v0.31.0)
//! renames without one; once a pinned cli-common syncs (v0.31.1), this is
//! `private_file::write(path, bytes, Replace::Always)`.
//!
//! [`Staged`] does the same for a file written as it streams: the bytes go
//! to the temp file, and only a commit after the last of them renames it
//! over the target.

use std::fs::File;
use std::path::{Path, PathBuf};

use pk_cli_core::CliError;
use pk_cli_drive::private_file::{self, Replace};

fn fail(path: &Path, what: &str, e: std::io::Error) -> CliError {
    CliError::Other(format!("{what} {}: {e}", path.display()))
}

/// The temp name beside `path`, in an owner-only directory it creates.
fn temp_beside(path: &Path) -> Result<PathBuf, CliError> {
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
    Ok(dir.join(format!(".{name}.{}.{nanos}.tmp", std::process::id())))
}

pub fn write_private(path: &Path, bytes: &[u8]) -> Result<(), CliError> {
    let fail = |what: &str, e: std::io::Error| fail(path, what, e);
    let tmp = temp_beside(path)?;
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

/// An owner-only file at `path`, written as it streams, that replaces what
/// is there only on [`Staged::commit`]. Until then the bytes go to a temp
/// file beside it, removed when the `Staged` is dropped uncommitted. A run
/// that fails or is killed partway leaves the previous file whole (a kill,
/// at worst, also leaves the hidden temp file).
pub struct Staged {
    path: PathBuf,
    tmp: PathBuf,
    file: Option<File>,
}

impl Staged {
    pub fn create(path: &Path) -> Result<Self, CliError> {
        let tmp = temp_beside(path)?;
        let mut open = std::fs::OpenOptions::new();
        // O_EXCL: a file or symbolic link planted at the temp name is
        // never opened.
        open.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            open.mode(0o600);
        }
        let file = open.open(&tmp).map_err(|e| fail(&tmp, "creating", e))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // The mode a umask cannot narrow to something else.
            if let Err(e) = file.set_permissions(std::fs::Permissions::from_mode(0o600)) {
                let _ = std::fs::remove_file(&tmp);
                return Err(fail(&tmp, "restricting", e));
            }
        }
        Ok(Staged {
            path: path.to_path_buf(),
            tmp,
            file: Some(file),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Where the bytes go until the commit.
    pub fn file(&mut self) -> &mut File {
        self.file
            .as_mut()
            .expect("a Staged file is open until its commit")
    }

    /// Sync the bytes and rename them over the target.
    pub fn commit(mut self) -> Result<(), CliError> {
        let file = self
            .file
            .take()
            .expect("a Staged file is open until its commit");
        let result = file
            .sync_all()
            .map_err(|e| fail(&self.path, "writing", e))
            .and_then(|()| {
                std::fs::rename(&self.tmp, &self.path).map_err(|e| fail(&self.path, "replacing", e))
            });
        if result.is_err() {
            let _ = std::fs::remove_file(&self.tmp);
        }
        result
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        if self.file.take().is_some() {
            let _ = std::fs::remove_file(&self.tmp);
        }
    }
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

    fn names(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn a_staged_file_replaces_the_old_one_only_when_committed() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("exp").join("records.ndjson");
        write_private(&path, b"previous, complete").unwrap();

        // Dropped partway (an error before the commit): the old file stays,
        // and no temp file is left.
        let mut staged = Staged::create(&path).unwrap();
        staged.file().write_all(b"half of a new").unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"previous, complete");
        drop(staged);
        assert_eq!(std::fs::read(&path).unwrap(), b"previous, complete");
        assert_eq!(names(path.parent().unwrap()), ["records.ndjson"]);

        let mut staged = Staged::create(&path).unwrap();
        staged.file().write_all(b"new, complete").unwrap();
        staged.commit().unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"new, complete");
        assert_eq!(names(path.parent().unwrap()), ["records.ndjson"]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_staged_file_replaces_a_symbolic_link_and_does_not_write_through_it() {
        use std::io::Write;
        let dir = tempfile::tempdir().unwrap();
        let elsewhere = dir.path().join("elsewhere");
        std::fs::write(&elsewhere, b"not ours").unwrap();
        let path = dir.path().join("records.ndjson");
        std::os::unix::fs::symlink(&elsewhere, &path).unwrap();
        let mut staged = Staged::create(&path).unwrap();
        staged.file().write_all(b"records").unwrap();
        staged.commit().unwrap();
        assert_eq!(std::fs::read(&elsewhere).unwrap(), b"not ours");
        assert!(!std::fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(std::fs::read(&path).unwrap(), b"records");
    }

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
