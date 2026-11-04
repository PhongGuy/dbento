//! Reading and atomically writing the app's JSON files (from Apinata).

use serde::Serialize;
use serde::de::DeserializeOwned;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

/// A loaded value and, when the file could not be used, a message for the user.
#[derive(Debug, Clone, PartialEq)]
pub struct Loaded<T> {
    /// The file's value, or the defaults.
    pub value: T,
    /// Set when defaults replaced an unreadable file; names the file, never its content.
    pub recovered: Option<String>,
}

/// Reads `path` as JSON. A missing file gives defaults silently; an unparsable one is
/// moved to `<name>.bad` and gives defaults with a message; another read error gives
/// defaults with a message and leaves the file alone.
pub fn load_json<T: DeserializeOwned + Default>(path: &Path) -> Loaded<T> {
    let name = file_name(path);
    let defaults = |recovered| Loaded {
        value: T::default(),
        recovered,
    };
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return defaults(None),
        Err(_) => {
            return defaults(Some(format!(
                "{name} could not be read; defaults are in use"
            )));
        }
    };
    match serde_json::from_str(&text) {
        Ok(value) => Loaded {
            value,
            recovered: None,
        },
        Err(_) => {
            // Set the corrupt file aside so the next save does not destroy what the
            // user may want to recover; an older `.bad` is replaced.
            let bad = path.with_file_name(format!("{name}.bad"));
            let message = match std::fs::rename(path, &bad) {
                Ok(()) => format!(
                    "{name} could not be read and was moved to {name}.bad; defaults are in use"
                ),
                Err(_) => format!("{name} could not be read; defaults are in use"),
            };
            defaults(Some(message))
        }
    }
}

/// Writes `value` as pretty JSON to `path`, creating the parent directory. The write
/// goes to a temporary sibling that is synced and renamed over `path`, so a reader
/// sees either the old file or the new one.
pub fn save_json<T: Serialize>(path: &Path, value: &T) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let mut file = tempfile::NamedTempFile::new_in(parent)?;
    serde_json::to_writer_pretty(&mut file, value)?;
    file.write_all(b"\n")?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|e| e.error)?;
    Ok(())
}

/// A change to the value of a `ChangeQueue`'s file.
pub type Change<T> = Box<dyn FnOnce(&mut T) + Send>;

/// Changes to one JSON file, queued from any thread and replayed in order onto the file as it
/// is now, so another process's edits to fields these changes leave alone survive.
pub struct ChangeQueue<T> {
    path: PathBuf,
    queued: Mutex<Vec<Change<T>>>,
    // Held through each load, replay and save, so batches reach the file in queue order; it
    // counts the writes.
    writing: Mutex<usize>,
}

impl<T: Serialize + DeserializeOwned + Default> ChangeQueue<T> {
    /// An empty queue for the file at `path`.
    pub fn new(path: PathBuf) -> Self {
        Self {
            path,
            queued: Mutex::default(),
            writing: Mutex::default(),
        }
    }

    /// Queues `change` for the next `write`.
    pub fn push(&self, change: Change<T>) {
        lock(&self.queued).push(change);
    }

    /// Whether any change waits for a write.
    pub fn pending(&self) -> bool {
        !lock(&self.queued).is_empty()
    }

    /// Loads the file, applies every queued change in order and saves it atomically; nothing
    /// queued writes nothing. A failed save drops the batch, as a failed write would.
    pub fn write(&self) -> io::Result<()> {
        let mut writes = lock(&self.writing);
        let batch = std::mem::take(&mut *lock(&self.queued));
        if batch.is_empty() {
            return Ok(());
        }
        let mut value = load_json::<T>(&self.path).value;
        for change in batch {
            change(&mut value);
        }
        save_json(&self.path, &value)?;
        *writes += 1;
        Ok(())
    }

    /// How many writes have reached the file.
    pub fn writes(&self) -> usize {
        *lock(&self.writing)
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

fn file_name(path: &Path) -> String {
    path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    )
}
