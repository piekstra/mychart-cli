//! Owner-only files, written whole: a temp file in the same directory,
//! `0600`, renamed over the target, so a reader never sees half a file and
//! nobody else on the machine can read it.

use std::io::Write;
use std::path::Path;

use pk_cli_core::CliError;

pub fn write_private(path: &Path, bytes: &[u8]) -> Result<(), CliError> {
    let fail =
        |what: &str, e: std::io::Error| CliError::Other(format!("{what} {}: {e}", path.display()));
    let dir = path.parent().unwrap_or(Path::new("."));
    create_private_dir(dir)?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".into());
    let tmp = dir.join(format!(".{name}.{}.tmp", std::process::id()));
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let result = (|| {
        let mut f = opts.open(&tmp).map_err(|e| fail("creating", e))?;
        f.write_all(bytes).map_err(|e| fail("writing", e))?;
        f.sync_all().map_err(|e| fail("writing", e))?;
        std::fs::rename(&tmp, path).map_err(|e| fail("replacing", e))
    })();
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
