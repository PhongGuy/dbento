//! `dbento run`: choosing the connection a saved SQL file runs on.

use std::path::Path;

use dbento_core::Workspace;

/// The connection whose `queries/<conn>/` folder holds `file`, judged by where the path the
/// user gave lies. Only the parent folder is resolved, so a symlink elsewhere cannot decide.
pub(crate) fn bound_connection(ws: &Workspace, file: &Path) -> anyhow::Result<Option<String>> {
    let parent = match file.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    let parent = parent
        .canonicalize()
        .map_err(|e| anyhow::anyhow!("cannot read {}: {e}", file.display()))?;
    for conn in ws.connections() {
        let folder = ws.dir().join("queries").join(conn.name());
        if folder.canonicalize().is_ok_and(|f| parent.starts_with(f)) {
            return Ok(Some(conn.name().to_owned()));
        }
    }
    Ok(None)
}
