//! Reloading the open workspace after outside changes: the per-process watching switch, the
//! off-thread disk read, and the pure verdicts on what a read means for the installed
//! dbento.toml and for an open saved-query tab.

use std::path::{Path, PathBuf};
use std::time::Duration;

use gpui_kit::{App, Global};
use similar::TextDiff;

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

/// What a saved-query tab knew of its file when a read began: the file and its known text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TabDisk {
    /// The saved query, relative to the workspace.
    pub file: PathBuf,
    /// The file's text as the tab last read or wrote it; None once kept after a delete.
    pub disk: Option<String>,
}

/// Why a saved-query tab's file needs the user: the banner over its editor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiskBanner {
    /// The file holds other text while the tab has edits of its own.
    Changed {
        /// The file's new text.
        disk: String,
    },
    /// The file is gone.
    Deleted,
    /// The file could not be read.
    Unreadable {
        /// Why, readable by a person.
        reason: String,
    },
}

/// What a read of a tab's file means for the tab.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TabVerdict {
    /// The tab saved, kept or moved while the read ran, so the read is out of date.
    Stale,
    /// The file holds what the tab knows; nothing to do.
    Same,
    /// The tab has no edits, so it takes the file's new text.
    Take(String),
    /// The user decides, through this banner.
    Show(DiskBanner),
}

/// Stale when `now` differs from `snapshot` (saved, kept or moved while the read ran). Text
/// equal to `disk` is Same; other text is Take without local edits, else Changed. Missing is
/// Same when `disk` is None, else Deleted. Unreadable always shows.
pub fn judge_tab(
    snapshot: &TabDisk,
    now: &TabDisk,
    local_edits: bool,
    read: &DiskRead,
) -> TabVerdict {
    if snapshot != now {
        return TabVerdict::Stale;
    }
    match read {
        DiskRead::Text(text) if now.disk.as_deref() == Some(text.as_str()) => TabVerdict::Same,
        DiskRead::Text(text) if !local_edits => TabVerdict::Take(text.clone()),
        DiskRead::Text(text) => TabVerdict::Show(DiskBanner::Changed { disk: text.clone() }),
        DiskRead::Missing if now.disk.is_none() => TabVerdict::Same,
        DiskRead::Missing => TabVerdict::Show(DiskBanner::Deleted),
        DiskRead::Unreadable(why) => TabVerdict::Show(DiskBanner::Unreadable {
            reason: why.clone(),
        }),
    }
}

/// A unified line diff of the disk text against the tab's, headed "--- disk" / "+++ this tab".
pub fn disk_diff(disk: &str, mine: &str) -> String {
    TextDiff::from_lines(disk, mine)
        .unified_diff()
        .context_radius(3)
        .header("disk", "this tab")
        .to_string()
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

/// Why a reload could not use dbento.toml when the file is gone.
pub const TOML_MISSING: &str = "the file is missing";
