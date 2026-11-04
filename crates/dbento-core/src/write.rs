//! The atomic replace of `dbento.toml` shared by edits and import.

use crate::edit::WriteError;
use std::io::{ErrorKind, Write};
use std::path::Path;

/// Replaces `file` with `text` through `dir/tmp_name` and a rename, keeping its permissions.
///
/// Refuses a symlink at `file`, and returns `Changed` unless `file` still holds `base`
/// (`None`: it must not exist).
pub(crate) fn replace_file(
    dir: &Path,
    file: &Path,
    base: Option<&str>,
    text: &str,
    tmp_name: &str,
) -> Result<(), WriteError> {
    let io = |path: &Path, e: std::io::Error| WriteError::Io {
        path: path.to_path_buf(),
        message: e.to_string(),
    };
    let meta = match std::fs::symlink_metadata(file) {
        Ok(m) if m.file_type().is_symlink() => {
            return Err(WriteError::Symlink {
                path: file.to_path_buf(),
            });
        }
        Ok(m) => Some(m),
        Err(e) if e.kind() == ErrorKind::NotFound => None,
        Err(e) => return Err(io(file, e)),
    };
    let now = match std::fs::read_to_string(file) {
        Ok(t) => Some(t),
        Err(e) if e.kind() == ErrorKind::NotFound => None,
        Err(e) => return Err(io(file, e)),
    };
    if now.as_deref() != base {
        return Err(WriteError::Changed);
    }

    // A leftover temporary file from a crashed run is ours to replace.
    let tmp = dir.join(tmp_name);
    match std::fs::remove_file(&tmp) {
        Err(e) if e.kind() != ErrorKind::NotFound => return Err(io(&tmp, e)),
        _ => {}
    }
    let mut out = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&tmp)
        .map_err(|e| io(&tmp, e))?;
    // The replacement keeps the old file's permissions, which may guard more than umask.
    let written = meta
        .map_or(Ok(()), |m| out.set_permissions(m.permissions()))
        .and_then(|()| out.write_all(text.as_bytes()))
        .and_then(|()| out.sync_all());
    drop(out);
    // A temporary file that never replaced `file` is ours to remove.
    if let Err(e) = written {
        let _ = std::fs::remove_file(&tmp);
        return Err(io(&tmp, e));
    }
    if let Err(e) = std::fs::rename(&tmp, file) {
        let _ = std::fs::remove_file(&tmp);
        return Err(io(file, e));
    }
    // Best effort: the rename has landed, so a failed folder sync is not worth an error.
    #[cfg(unix)]
    let _ = std::fs::File::open(dir).and_then(|d| d.sync_all());
    Ok(())
}
