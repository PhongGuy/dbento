//! One DuckDB instance per canonical file path, shared by every session as a lease.
//!
//! DuckDB loses data when one process opens a file twice, so every session on a file
//! clones the one root `Connection` the registry keeps, and the instance closes when the
//! last lease drops.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{LazyLock, Mutex, MutexGuard, PoisonError};

use ::duckdb::{AccessMode, Config, Connection};

use crate::session::ConnectError;

#[cfg(test)]
mod tests;

/// How an instance is opened: writable, or read-only with external access and
/// configuration locked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mode {
    ReadWrite,
    Locked,
}

/// One open file: the root connection every lease clones, its mode and its live leases.
struct Entry {
    root: Connection,
    mode: Mode,
    sessions: usize,
}

static OPEN: LazyLock<Mutex<HashMap<PathBuf, Entry>>> = LazyLock::new(Default::default);

// A panic while the map was held leaves it consistent (every mutation is one call), so a
// poisoned lock is taken over rather than spreading the panic to every later open.
fn open_files() -> MutexGuard<'static, HashMap<PathBuf, Entry>> {
    OPEN.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A session's share of a file's instance; dropping it ends the share.
pub(crate) struct Lease {
    conn: Option<Connection>,
    key: PathBuf,
}

impl Lease {
    /// The session's own connection to the shared instance.
    #[allow(clippy::expect_used)] // only Drop takes the connection out
    pub(crate) fn conn(&self) -> &Connection {
        self.conn
            .as_ref()
            .expect("a live lease holds its connection")
    }
}

/// Opens `path` in `mode`, sharing the instance already open on it in this process.
///
/// A missing file is `NotFound` and is never created; the file open in the other mode
/// here, or locked by another process, and any other DuckDB failure are `Open`.
pub(crate) fn open(path: &Path, mode: Mode) -> Result<Lease, ConnectError> {
    if !path.exists() {
        return Err(ConnectError::NotFound {
            path: path.to_owned(),
        });
    }
    let key = path.canonicalize().map_err(|e| ConnectError::Open {
        message: format!("{}: {e}", path.display()),
    })?;
    // One lock across lookup and open: two sessions racing on a new file get one instance.
    let mut open = open_files();
    if let Some(entry) = open.get_mut(&key) {
        if entry.mode != mode {
            return Err(conflict(path, entry.mode));
        }
        let conn = entry.root.try_clone().map_err(|e| open_error(path, &e))?;
        entry.sessions += 1;
        return Ok(Lease {
            conn: Some(conn),
            key,
        });
    }
    let root =
        Connection::open_with_flags(&key, config(mode)?).map_err(|e| open_error(path, &e))?;
    let conn = root.try_clone().map_err(|e| open_error(path, &e))?;
    open.insert(
        key.clone(),
        Entry {
            root,
            mode,
            sessions: 1,
        },
    );
    Ok(Lease {
        conn: Some(conn),
        key,
    })
}

impl Drop for Lease {
    // Clone first, root last, both under the lock: the instance is closed and its file lock
    // released before any other open of the path can start.
    fn drop(&mut self) {
        let mut open = open_files();
        drop(self.conn.take());
        if let Some(entry) = open.get_mut(&self.key) {
            entry.sessions -= 1;
            if entry.sessions == 0 {
                open.remove(&self.key);
            }
        }
    }
}

// No autoload and no writes under `$HOME`; a locked instance also gets no file access and
// no `SET`, all fixed at open so no session can undo them.
fn config(mode: Mode) -> Result<Config, ConnectError> {
    let home = home_dir()?;
    let home = home.to_string_lossy();
    let cfg = Config::default()
        .enable_autoload_extension(false)
        .and_then(|c| c.with("home_directory", &*home))
        .and_then(|c| c.with("extension_directory", &*home));
    let cfg = match mode {
        Mode::ReadWrite => cfg.and_then(|c| c.access_mode(AccessMode::ReadWrite)),
        Mode::Locked => cfg
            .and_then(|c| c.access_mode(AccessMode::ReadOnly))
            .and_then(|c| c.enable_external_access(false))
            .and_then(|c| c.with("lock_configuration", "true")),
    };
    cfg.map_err(|e| ConnectError::Open {
        message: e.to_string(),
    })
}

/// DuckDB's home and extension directory, created if missing.
fn home_dir() -> Result<PathBuf, ConnectError> {
    let dir = dirs::cache_dir()
        .map(|d| d.join("dbento").join("duckdb"))
        .unwrap_or_else(|| std::env::temp_dir().join("dbento-duckdb"));
    std::fs::create_dir_all(&dir).map_err(|e| ConnectError::Open {
        message: format!("{}: {e}", dir.display()),
    })?;
    Ok(dir)
}

fn conflict(path: &Path, held: Mode) -> ConnectError {
    let p = path.display();
    let message = match held {
        Mode::ReadWrite => {
            format!(
                "`{p}` is already open read-write in this process; a read-only session needs it closed"
            )
        }
        Mode::Locked => {
            format!(
                "`{p}` is already open read-only in this process; a writable session needs it closed"
            )
        }
    };
    ConnectError::Open { message }
}

fn open_error(path: &Path, e: &::duckdb::Error) -> ConnectError {
    let text = e.to_string();
    let message = lock_message(path, &text).unwrap_or(text);
    ConnectError::Open { message }
}

/// DuckDB's cross-process lock texts as Dbento's message; None for any other text.
pub(crate) fn lock_message(path: &Path, duckdb_text: &str) -> Option<String> {
    if !duckdb_text.contains("Could not set lock on file") {
        return None;
    }
    let p = path.display();
    // DuckDB adds the read-only hint only when the other process holds a shared lock.
    Some(
        if duckdb_text.contains("you would be able to open this database in read-only mode") {
            format!(
                "`{p}` is open read-only in another process; DuckDB opens it for writing only when no other process has it"
            )
        } else {
            format!("`{p}` is open read-write in another process (for example the Dbento app)")
        },
    )
}
