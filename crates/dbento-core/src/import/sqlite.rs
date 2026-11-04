//! The SQLite path tail shared by the importers, run once each has stripped its own syntax.

use crate::import::model::{ConnectionDraft, SkipReason, WarningKind};
use crate::import::plan::is_windows_path;
use std::path::Path;

#[cfg(test)]
mod tests;

/// The SQLite tail shared by the importers. `path` is already free of source syntax; each
/// `dropped` name warns once as `DroppedParameter`, in first-seen order. "" or ":memory:" is
/// `InMemory`; a Windows path is kept verbatim; any other path is `root.join(path)`; a path
/// that does not exist adds `FileMissing` last.
pub(crate) fn map_path(
    path: &str,
    dropped: &[&str],
    root: &Path,
    read_only: bool,
) -> Result<(ConnectionDraft, Vec<WarningKind>), SkipReason> {
    // Dbento has no per-connection SQLite options, so every parameter and property drops.
    let mut warnings: Vec<WarningKind> = Vec::new();
    for name in dropped {
        let w = WarningKind::DroppedParameter((*name).to_owned());
        if !warnings.contains(&w) {
            warnings.push(w);
        }
    }
    if path.is_empty() || path == ":memory:" {
        return Err(SkipReason::InMemory);
    }
    let path = if is_windows_path(path) {
        path.to_owned()
    } else {
        root.join(path).to_string_lossy().into_owned()
    };
    if !Path::new(&path).exists() {
        warnings.push(WarningKind::FileMissing);
    }
    Ok((ConnectionDraft::sqlite(path, read_only), warnings))
}
