//! Where Dbento keeps its own files outside any workspace, shared by the app and the MCP
//! server so both write the same history and read the same approvals.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

/// The environment variable that moves every state file into one folder, as `make demo` does.
pub const STATE_DIR_VAR: &str = "DBENTO_STATE_DIR";

/// The locations of Dbento's state files.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatePaths {
    /// The app's window and workspace state, as JSON.
    pub state: PathBuf,
    /// The query history, one JSON object per line.
    pub history: PathBuf,
    /// The folder of unsaved query drafts.
    pub drafts: PathBuf,
    /// The folder of pending MCP approvals.
    pub approvals: PathBuf,
    /// The user's settings, as JSON.
    pub settings: PathBuf,
    /// The user's key binding overrides, as JSON.
    pub keymap: PathBuf,
}

impl StatePaths {
    /// The platform locations: state, settings and keymap under the config dir, history,
    /// drafts and approvals under the data dir, each in a `dbento` folder. `None` without a
    /// home directory.
    pub fn platform() -> Option<StatePaths> {
        let config = dirs::config_dir()?.join("dbento");
        let data = dirs::data_dir()?.join("dbento");
        Some(StatePaths {
            state: config.join("state.json"),
            history: data.join("history.jsonl"),
            drafts: data.join("drafts"),
            approvals: data.join("approvals"),
            settings: config.join("settings.json"),
            keymap: config.join("keymap.json"),
        })
    }

    /// Every file directly under `dir`, for tests and portable setups.
    pub fn in_dir(dir: &Path) -> StatePaths {
        StatePaths {
            state: dir.join("state.json"),
            history: dir.join("history.jsonl"),
            drafts: dir.join("drafts"),
            approvals: dir.join("approvals"),
            settings: dir.join("settings.json"),
            keymap: dir.join("keymap.json"),
        }
    }

    /// [`StatePaths::in_dir`] of `state_dir` when it is set and non-empty; otherwise
    /// [`StatePaths::platform`].
    pub fn for_state_dir(state_dir: Option<OsString>) -> Option<StatePaths> {
        match state_dir.filter(|d| !d.is_empty()) {
            Some(dir) => Some(StatePaths::in_dir(Path::new(&dir))),
            None => StatePaths::platform(),
        }
    }

    /// [`StatePaths::for_state_dir`] of the [`STATE_DIR_VAR`] environment variable.
    pub fn from_env() -> Option<StatePaths> {
        StatePaths::for_state_dir(std::env::var_os(STATE_DIR_VAR))
    }
}

/// The history file under [`StatePaths::from_env`].
pub fn history_file() -> Option<PathBuf> {
    StatePaths::from_env().map(|p| p.history)
}

/// The approvals folder under [`StatePaths::from_env`].
pub fn approvals_dir() -> Option<PathBuf> {
    StatePaths::from_env().map(|p| p.approvals)
}
