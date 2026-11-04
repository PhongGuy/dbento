//! Writing a result's loaded rows to a file as CSV or JSON; nothing re-runs.

use std::fmt::Display;
use std::io::Write as _;
use std::path::{Path, PathBuf};

use crate::copy::{CopyFormat, copy_rows};
use crate::results::ResultStore;

/// The file format an export writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    /// Comma-separated with a header line.
    Csv,
    /// An array of objects keyed by column name.
    Json,
}

/// The format `path`'s extension asks for: `.json` in any case is JSON, anything else CSV.
pub fn export_format(path: &Path) -> ExportFormat {
    match path.extension() {
        Some(ext) if ext.eq_ignore_ascii_case("json") => ExportFormat::Json,
        _ => ExportFormat::Csv,
    }
}

/// What the save dialog's `reply` leaves to export: the chosen path, None when it was cancelled
/// or dropped, or the notice text when the dialog itself failed.
pub fn chosen_path<E: Display, C>(
    reply: Result<Result<Option<PathBuf>, E>, C>,
) -> Result<Option<PathBuf>, String> {
    match reply {
        Ok(Ok(path)) => Ok(path),
        Ok(Err(e)) => Err(format!("Export failed: {e}")),
        Err(_) => Ok(None),
    }
}

/// `copy::copy_rows` over every loaded row and column, with headers.
pub fn export_text(store: &ResultStore, format: ExportFormat) -> String {
    let format = match format {
        ExportFormat::Csv => CopyFormat::Csv,
        ExportFormat::Json => CopyFormat::Json,
    };
    let cols: Vec<usize> = (0..store.columns().len()).collect();
    copy_rows(store, 0..store.rows(), &cols, &format, true)
}

/// Writes `text` to a temp sibling of `path`, then renames it over `path`, so a reader never
/// sees half a file and a failed write leaves the old one whole. A new file gets the umask's
/// mode like `File::create`; an overwritten one keeps its mode.
pub fn write_export(path: &Path, text: &str) -> std::io::Result<()> {
    let dir = match path.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    // A temp file defaults to 0600, which the rename would carry over to the export, so
    // ask for 0666 through the umask and then copy any existing file's mode onto it.
    #[cfg_attr(not(unix), allow(unused_mut))]
    let mut builder = tempfile::Builder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        builder.permissions(std::fs::Permissions::from_mode(0o666));
    }
    let mut tmp = builder.tempfile_in(dir)?;
    #[cfg(unix)]
    if let Ok(meta) = std::fs::metadata(path) {
        tmp.as_file().set_permissions(meta.permissions())?;
    }
    tmp.write_all(text.as_bytes())?;
    tmp.as_file().sync_all()?;
    tmp.persist(path).map_err(|e| e.error)?;
    Ok(())
}
