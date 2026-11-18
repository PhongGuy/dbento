//! Reloading the open workspace after outside changes: the per-process watching switch, the
//! off-thread disk read, and the pure verdict on what that read means for the installed file.

use std::path::Path;
use std::time::Duration;

use gpui_kit::{App, Global};

/// How long a reload waits before rereading a file it could not use the first time.
pub const TOML_RETRY: Duration = Duration::from_millis(250);

// Present once the process has asked for watching; test processes never set it, so their
// own writes to a fixture never race a watcher.
struct Watching;

impl Global for Watching {}

/// Makes every workspace this process opens from now on watched for outside changes.
pub fn enable_watching(cx: &mut App) {
    cx.set_global(Watching);
}

/// Whether `enable_watching` was called in this process.
pub fn watching_enabled(cx: &App) -> bool {
    cx.has_global::<Watching>()
}

/// What reading one workspace file found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiskRead {
    /// The file's text.
    Text(String),
    /// No file at the path.
    Missing,
    /// The file is there but could not be read; the reason is readable by a person.
    Unreadable(String),
}

/// What a dbento.toml read means for the installed workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TomlVerdict {
    /// The app installed another workspace while the read ran, so the read is out of date.
    Stale,
    /// The file holds the installed text; nothing changed.
    Same,
    /// The file holds other text, to be parsed and installed.
    Parse(String),
    /// The file is gone.
    Missing,
    /// The file could not be read, for the given reason.
    Unreadable(String),
}

/// Stale when `installed` no longer equals `snapshot` (the app wrote meanwhile); else Same
/// for equal text, Parse for other text, and the read's own Missing or Unreadable.
pub fn judge_toml(snapshot: &str, installed: &str, read: &DiskRead) -> TomlVerdict {
    if snapshot != installed {
        return TomlVerdict::Stale;
    }
    match read {
        DiskRead::Text(text) if text == installed => TomlVerdict::Same,
        DiskRead::Text(text) => TomlVerdict::Parse(text.clone()),
        DiskRead::Missing => TomlVerdict::Missing,
        DiskRead::Unreadable(why) => TomlVerdict::Unreadable(why.clone()),
    }
}

/// Reads `path` as text: a missing file is `Missing`, any other failure `Unreadable`.
pub fn read_disk(path: &Path) -> DiskRead {
    match std::fs::read_to_string(path) {
        Ok(text) => DiskRead::Text(text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => DiskRead::Missing,
        Err(e) => DiskRead::Unreadable(e.to_string()),
    }
}

/// The banner line for a dbento.toml that could not be used, given why.
pub fn toml_problem_text(reason: &str) -> String {
    let reason = reason.trim_end_matches('.');
    format!("dbento.toml has an error: {reason}. Showing the last version that loaded.")
}

/// The banner line for a workspace folder that is not being watched, given why.
pub fn watch_problem_text(reason: &str) -> String {
    let reason = reason.trim_end_matches('.');
    format!("Not watching this folder for changes: {reason}. Use Reload after changing files.")
}
