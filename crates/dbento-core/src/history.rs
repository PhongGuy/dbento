//! The query history: a JSON Lines file with one entry per statement the app ran.
//!
//! Each entry is a single append under a shared `flock`; compaction rewrites the file under
//! the exclusive lock and renames it into place. Readers need no lock, and a torn last line
//! from a crashed writer is skipped like any other corrupt line. Entries hold the connection's name and the
//! workspace folder, never a URL, host, user or secret.

use serde::{Deserialize, Serialize};
use std::fs::{File, OpenOptions};
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Compaction runs once the file holds more lines than this.
pub const HISTORY_COMPACT_LINES: usize = 40_000;
/// The number of newest lines a compaction keeps.
pub const HISTORY_KEEP_LINES: usize = 20_000;
/// Compaction also runs once the file grows past this many bytes.
pub const HISTORY_COMPACT_BYTES: u64 = 32 << 20;
/// A compaction keeps the newest entries that fit in this many bytes.
pub const HISTORY_KEEP_BYTES: u64 = 16 << 20;
/// An entry's SQL is cut to this many bytes.
pub const HISTORY_MAX_SQL_BYTES: usize = 64 << 10;
/// A failure message is cut to this many bytes.
pub const HISTORY_MAX_MESSAGE_BYTES: usize = 4 << 10;

/// `recent` reads at most the newest this-many bytes, so a file grown far past the compaction
/// bound by something else cannot exhaust memory.
const READ_CAP_BYTES: u64 = 2 * HISTORY_COMPACT_BYTES;
const VERSION: u32 = 1;

/// An open history file that entries are appended to.
#[non_exhaustive]
#[derive(Debug)]
pub struct History {
    path: PathBuf,
    file: File,
    lines: usize,
}

/// One statement the app ran, as the history file keeps it.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HistoryEntry {
    at: SystemTime,
    workspace: PathBuf,
    connection: String,
    sql: String,
    elapsed: Duration,
    outcome: HistoryOutcome,
    source: HistorySource,
}

/// Which shell ran a recorded statement.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum HistorySource {
    /// The desktop app.
    #[default]
    App,
    /// An AI client through the MCP server.
    Mcp,
}

/// How a recorded statement ended.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HistoryOutcome {
    /// The statement completed.
    Ok {
        /// Rows collected.
        rows: u64,
        /// Whether collection stopped at a limit.
        truncated: bool,
        /// Rows the statement changed, when the database reports it.
        rows_affected: Option<u64>,
    },
    /// The statement failed.
    Failed {
        /// The error, cut to [`HISTORY_MAX_MESSAGE_BYTES`].
        message: String,
    },
    /// The statement was stopped before it finished.
    Cancelled,
    /// The statement never ran.
    Skipped,
}

/// Which entries [`History::recent`] returns; every unset field matches everything.
#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HistoryFilter {
    workspace: Option<PathBuf>,
    connection: Option<String>,
    text: Option<String>,
}

/// Why the history file could not be used.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum HistoryError {
    /// The file could not be opened, read or written, or is not a regular file.
    #[error("cannot use the history file {}: {message}", path.display())]
    Io {
        /// The history file.
        path: PathBuf,
        /// What went wrong.
        message: String,
    },
    /// Another process holds the lock this operation needs.
    #[error("the history file {} is locked by another process", path.display())]
    Locked {
        /// The history file.
        path: PathBuf,
    },
}

enum Lock {
    Shared,
    Exclusive,
}

#[derive(Serialize, Deserialize)]
struct Line {
    v: u32,
    at_ms: u64,
    workspace: String,
    connection: String,
    sql: String,
    elapsed_us: u64,
    outcome: RawOutcome,
    // Lines written before the source existed read as the app's.
    #[serde(default)]
    source: RawSource,
}

#[derive(Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
enum RawSource {
    #[default]
    App,
    Mcp,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum RawOutcome {
    Ok {
        rows: u64,
        truncated: bool,
        rows_affected: Option<u64>,
    },
    Failed {
        message: String,
    },
    Cancelled,
    Skipped,
}

impl History {
    /// Opens or creates (0600, O_APPEND, parent folders created) the file at `path`; Io for a
    /// symlink at `path` or any I/O error. Counts its lines for the compaction trigger.
    pub fn open(path: &Path) -> Result<History, HistoryError> {
        let io = |e: std::io::Error| io_error(path, e.to_string());
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(io)?;
        }
        let file = open_regular(path, true)?;
        let lines = count_lines(&file, path)?;
        Ok(History {
            path: path.to_owned(),
            file,
            lines,
        })
    }

    /// Writes `entry` as one line under a shared lock, then compacts when the file passes
    /// HISTORY_COMPACT_LINES or HISTORY_COMPACT_BYTES; a compaction that fails or meets
    /// Locked is skipped and retried on a later append.
    pub fn append(&mut self, entry: &HistoryEntry) -> Result<(), HistoryError> {
        let mut line = serde_json::to_string(&Line::from(entry))
            .map_err(|e| io_error(&self.path, e.to_string()))?;
        line.push('\n');
        self.lock_current(Lock::Shared)?;
        let written = self.write_line(line);
        let _ = self.file.unlock();
        written?;
        let len = self.file.metadata().map_or(0, |m| m.len());
        if self.lines > HISTORY_COMPACT_LINES || len > HISTORY_COMPACT_BYTES {
            let _ = self.compact();
        }
        Ok(())
    }

    /// Keeps the newest valid entries within both keep limits; Locked when another handle
    /// holds any lock; a no-op when the file is already within both compaction limits.
    pub fn compact(&mut self) -> Result<(), HistoryError> {
        self.lock_current(Lock::Exclusive)?;
        let rewritten = self.rewrite();
        // After a rewrite this handle is the new, unlocked file and the old one is closed.
        let _ = self.file.unlock();
        rewritten
    }

    /// Under the exclusive lock nobody appends, so the file read here is the whole history.
    /// The kept lines go to a 0600 temp file renamed over the path; appenders waiting on the
    /// old inode see the rename in `lock_current` and follow it.
    fn rewrite(&mut self) -> Result<(), HistoryError> {
        let io = |e: std::io::Error| io_error(&self.path, e.to_string());
        let len = self.file.metadata().map_err(io)?.len();
        let lines = count_lines(&self.file, &self.path)?;
        if lines <= HISTORY_COMPACT_LINES && len <= HISTORY_COMPACT_BYTES {
            self.lines = lines;
            return Ok(());
        }
        // Any line that fits the byte budget lies wholly within the last KEEP_BYTES + 1 bytes,
        // newline before it included, so nothing older needs reading.
        let start = len.saturating_sub(HISTORY_KEEP_BYTES + 1);
        let mut tail = Vec::new();
        let mut reader = &self.file;
        reader.seek(SeekFrom::Start(start)).map_err(io)?;
        reader
            .take(len - start)
            .read_to_end(&mut tail)
            .map_err(io)?;
        if start > 0 {
            let first = tail
                .iter()
                .position(|&b| b == b'\n')
                .map_or(tail.len(), |i| i + 1);
            tail.drain(..first);
        }
        let mut kept: Vec<&[u8]> = Vec::new();
        let mut bytes = 0;
        for l in tail.split(|&b| b == b'\n').rev() {
            if kept.len() == HISTORY_KEEP_LINES {
                break;
            }
            if !serde_json::from_slice::<Line>(l).is_ok_and(|l| l.v == VERSION) {
                continue;
            }
            if bytes + l.len() as u64 + 1 > HISTORY_KEEP_BYTES {
                break;
            }
            bytes += l.len() as u64 + 1;
            kept.push(l);
        }
        let mut out = Vec::with_capacity(bytes as usize);
        for l in kept.iter().rev() {
            out.extend_from_slice(l);
            out.push(b'\n');
        }
        let folder = match self.path.parent() {
            Some(p) if !p.as_os_str().is_empty() => p,
            _ => Path::new("."),
        };
        let mut tmp = tempfile::NamedTempFile::new_in(folder).map_err(io)?;
        tmp.write_all(&out).map_err(io)?;
        tmp.as_file().sync_all().map_err(io)?;
        tmp.persist(&self.path).map_err(|e| io(e.error))?;
        self.file = open_regular(&self.path, true)?;
        self.lines = kept.len();
        Ok(())
    }

    /// Takes `lock` on the file now at the path. A compaction may have renamed a new file into
    /// place after this handle opened; then reopen and lock again, so no line lands in the old
    /// inode. A reopen recounts lines, since the new file's count drives the next trigger.
    fn lock_current(&mut self, lock: Lock) -> Result<(), HistoryError> {
        let io = |path: &Path, e: std::io::Error| io_error(path, e.to_string());
        loop {
            match lock {
                Lock::Shared => self.file.lock_shared().map_err(|e| io(&self.path, e))?,
                Lock::Exclusive => match self.file.try_lock() {
                    Ok(()) => {}
                    Err(std::fs::TryLockError::WouldBlock) => {
                        return Err(HistoryError::Locked {
                            path: self.path.clone(),
                        });
                    }
                    Err(std::fs::TryLockError::Error(e)) => return Err(io(&self.path, e)),
                },
            }
            match still_at_path(&self.file, &self.path) {
                Ok(true) => return Ok(()),
                Ok(false) => {}
                Err(e) => {
                    let _ = self.file.unlock();
                    return Err(io(&self.path, e));
                }
            }
            let _ = self.file.unlock();
            self.file = open_regular(&self.path, true)?;
            self.lines = count_lines(&self.file, &self.path)?;
        }
    }

    /// Appends `line`, the caller holding the shared lock.
    fn write_line(&mut self, mut line: String) -> Result<(), HistoryError> {
        let io = |e: std::io::Error| io_error(&self.path, e.to_string());
        // A crashed writer may have left a torn last line; start on a fresh line so it stays
        // one skipped corrupt line instead of swallowing this entry.
        if self.file.metadata().map_err(io)?.len() > 0 {
            let mut last = [0u8; 1];
            self.file.seek(SeekFrom::End(-1)).map_err(io)?;
            self.file.read_exact(&mut last).map_err(io)?;
            if last[0] != b'\n' {
                line.insert(0, '\n');
                self.lines += 1;
            }
        }
        self.file.write_all(line.as_bytes()).map_err(io)?;
        self.lines += 1;
        Ok(())
    }

    /// Entries matching `filter`, newest appended first, at most `limit`; corrupt lines,
    /// other versions and read errors give nothing.
    pub fn recent(&self, filter: &HistoryFilter, limit: usize) -> Vec<HistoryEntry> {
        if limit == 0 {
            return Vec::new();
        }
        let Ok(bytes) = read_tail(&self.path) else {
            return Vec::new();
        };
        let needle = filter.text.as_deref().map(str::to_lowercase);
        String::from_utf8_lossy(&bytes)
            .lines()
            .rev()
            .filter_map(|l| serde_json::from_str::<Line>(l).ok())
            .filter(|l| l.v == VERSION)
            .map(HistoryEntry::from)
            .filter(|e| filter.workspace.as_ref().is_none_or(|w| *w == e.workspace))
            .filter(|e| {
                filter
                    .connection
                    .as_ref()
                    .is_none_or(|c| *c == e.connection)
            })
            .filter(|e| {
                needle
                    .as_ref()
                    .is_none_or(|n| e.sql.to_lowercase().contains(n))
            })
            .take(limit)
            .collect()
    }

    /// The history file's path.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl HistoryEntry {
    /// `at` truncated to milliseconds and `elapsed` to microseconds (what the file keeps);
    /// `sql` cut to HISTORY_MAX_SQL_BYTES and a Failed message to HISTORY_MAX_MESSAGE_BYTES.
    pub fn new(
        at: SystemTime,
        workspace: &Path,
        connection: &str,
        sql: &str,
        elapsed: Duration,
        outcome: HistoryOutcome,
    ) -> HistoryEntry {
        let outcome = match outcome {
            HistoryOutcome::Failed { message } => HistoryOutcome::Failed {
                message: cut(&message, HISTORY_MAX_MESSAGE_BYTES).to_owned(),
            },
            other => other,
        };
        HistoryEntry {
            at: UNIX_EPOCH + Duration::from_millis(millis(at)),
            workspace: PathBuf::from(workspace.to_string_lossy().into_owned()),
            connection: connection.to_owned(),
            sql: cut(sql, HISTORY_MAX_SQL_BYTES).to_owned(),
            elapsed: Duration::from_micros(micros(elapsed)),
            outcome,
            source: HistorySource::App,
        }
    }

    /// This entry recorded as run by `source`.
    pub fn with_source(mut self, source: HistorySource) -> HistoryEntry {
        self.source = source;
        self
    }

    /// Which shell ran the statement; [`HistorySource::App`] unless set.
    pub fn source(&self) -> HistorySource {
        self.source
    }

    /// When the statement started.
    pub fn at(&self) -> SystemTime {
        self.at
    }

    /// The workspace folder the statement ran in.
    pub fn workspace(&self) -> &Path {
        &self.workspace
    }

    /// The name of the connection the statement ran on.
    pub fn connection(&self) -> &str {
        &self.connection
    }

    /// The statement's SQL text.
    pub fn sql(&self) -> &str {
        &self.sql
    }

    /// How long the statement executed.
    pub fn elapsed(&self) -> Duration {
        self.elapsed
    }

    /// How the statement ended.
    pub fn outcome(&self) -> &HistoryOutcome {
        &self.outcome
    }
}

impl HistoryFilter {
    /// Keeps only entries from `workspace`.
    pub fn workspace(mut self, workspace: &Path) -> Self {
        self.workspace = Some(workspace.to_owned());
        self
    }

    /// Keeps only entries on the connection named `connection`.
    pub fn connection(mut self, connection: &str) -> Self {
        self.connection = Some(connection.to_owned());
        self
    }

    /// Keeps only entries whose SQL contains `text`, ignoring case.
    pub fn text(mut self, text: &str) -> Self {
        self.text = Some(text.to_owned());
        self
    }
}

impl From<&HistoryEntry> for Line {
    fn from(e: &HistoryEntry) -> Line {
        let outcome = match &e.outcome {
            HistoryOutcome::Ok {
                rows,
                truncated,
                rows_affected,
            } => RawOutcome::Ok {
                rows: *rows,
                truncated: *truncated,
                rows_affected: *rows_affected,
            },
            HistoryOutcome::Failed { message } => RawOutcome::Failed {
                message: message.clone(),
            },
            HistoryOutcome::Cancelled => RawOutcome::Cancelled,
            HistoryOutcome::Skipped => RawOutcome::Skipped,
        };
        Line {
            v: VERSION,
            at_ms: millis(e.at),
            workspace: e.workspace.to_string_lossy().into_owned(),
            connection: e.connection.clone(),
            sql: e.sql.clone(),
            elapsed_us: micros(e.elapsed),
            outcome,
            source: match e.source {
                HistorySource::App => RawSource::App,
                HistorySource::Mcp => RawSource::Mcp,
            },
        }
    }
}

impl From<Line> for HistoryEntry {
    fn from(l: Line) -> HistoryEntry {
        let outcome = match l.outcome {
            RawOutcome::Ok {
                rows,
                truncated,
                rows_affected,
            } => HistoryOutcome::Ok {
                rows,
                truncated,
                rows_affected,
            },
            RawOutcome::Failed { message } => HistoryOutcome::Failed { message },
            RawOutcome::Cancelled => HistoryOutcome::Cancelled,
            RawOutcome::Skipped => HistoryOutcome::Skipped,
        };
        HistoryEntry {
            at: UNIX_EPOCH + Duration::from_millis(l.at_ms),
            workspace: PathBuf::from(l.workspace),
            connection: l.connection,
            sql: l.sql,
            elapsed: Duration::from_micros(l.elapsed_us),
            outcome,
            source: match l.source {
                RawSource::App => HistorySource::App,
                RawSource::Mcp => HistorySource::Mcp,
            },
        }
    }
}

fn io_error(path: &Path, message: String) -> HistoryError {
    HistoryError::Io {
        path: path.to_owned(),
        message,
    }
}

fn millis(at: SystemTime) -> u64 {
    let since = at.duration_since(UNIX_EPOCH).unwrap_or_default();
    u64::try_from(since.as_millis()).unwrap_or(u64::MAX)
}

fn micros(d: Duration) -> u64 {
    u64::try_from(d.as_micros()).unwrap_or(u64::MAX)
}

fn cut(s: &str, max: usize) -> &str {
    &s[..s.floor_char_boundary(max)]
}

/// Opens `path` without following a symlink at it: an exclusive create cannot go through one,
/// and an existing file must be the same regular file before and after the open.
fn open_regular(path: &Path, append: bool) -> Result<File, HistoryError> {
    let io = |e: std::io::Error| io_error(path, e.to_string());
    let mut options = OpenOptions::new();
    options.read(true).append(append);
    // O_NOFOLLOW refuses a symlink swapped in after the check below, and O_NONBLOCK keeps a
    // swapped-in FIFO from blocking the open before the regular-file check rejects it.
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    if append {
        match options.clone().create_new(true).open(path) {
            Ok(file) => return Ok(file),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {}
            Err(e) => return Err(io(e)),
        }
    }
    let before = std::fs::symlink_metadata(path).map_err(io)?;
    if !before.file_type().is_file() {
        return Err(io_error(path, "not a regular file".into()));
    }
    let file = options.open(path).map_err(io)?;
    let after = file.metadata().map_err(io)?;
    if !after.file_type().is_file() || !same_file(&before, &after) {
        return Err(io_error(
            path,
            "the file changed while it was opened".into(),
        ));
    }
    Ok(file)
}

#[cfg(unix)]
fn same_file(a: &std::fs::Metadata, b: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt as _;
    (a.dev(), a.ino()) == (b.dev(), b.ino())
}

#[cfg(not(unix))]
fn same_file(_: &std::fs::Metadata, _: &std::fs::Metadata) -> bool {
    true
}

/// Whether `file` is still the file at `path` (same device and inode); a missing path is
/// "not the same", so the reopen recreates it.
#[cfg(unix)]
fn still_at_path(file: &File, path: &Path) -> std::io::Result<bool> {
    let a = file.metadata()?;
    match std::fs::symlink_metadata(path) {
        Ok(b) => Ok(same_file(&a, &b)),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

#[cfg(not(unix))]
fn still_at_path(_: &File, _: &Path) -> std::io::Result<bool> {
    Ok(true)
}

/// The number of newline bytes in `file`, read from its start.
fn count_lines(file: &File, path: &Path) -> Result<usize, HistoryError> {
    let io = |e: std::io::Error| io_error(path, e.to_string());
    let mut reader = file;
    reader.seek(SeekFrom::Start(0)).map_err(io)?;
    let mut lines = 0;
    let mut buf = vec![0u8; 64 << 10];
    loop {
        match reader.read(&mut buf) {
            Ok(0) => return Ok(lines),
            Ok(n) => lines += buf[..n].iter().filter(|&&b| b == b'\n').count(),
            Err(e) if e.kind() == ErrorKind::Interrupted => {}
            Err(e) => return Err(io(e)),
        }
    }
}

/// The newest READ_CAP_BYTES of the file at `path`, starting on a line boundary when cut.
fn read_tail(path: &Path) -> Result<Vec<u8>, HistoryError> {
    let io = |e: std::io::Error| io_error(path, e.to_string());
    let mut file = open_regular(path, false)?;
    let len = file.metadata().map_err(io)?.len();
    let start = len.saturating_sub(READ_CAP_BYTES);
    file.seek(SeekFrom::Start(start)).map_err(io)?;
    let mut bytes = Vec::new();
    file.take(READ_CAP_BYTES)
        .read_to_end(&mut bytes)
        .map_err(io)?;
    if start > 0 {
        let first = bytes
            .iter()
            .position(|&b| b == b'\n')
            .map_or(bytes.len(), |i| i + 1);
        bytes.drain(..first);
    }
    Ok(bytes)
}
