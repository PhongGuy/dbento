//! The process's parked SQLite reads, by database file. A parked read holds its file's shared
//! lock, which fails a COMMIT on that file, so a commit asks every read parked there to let go
//! first (see `release`). Postgres reads never block a commit and are not registered.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex, PoisonError};
use std::time::Duration;

use tokio::sync::Notify;

/// One registered read: the flag a release sets and the wake-up for a read waiting on it.
#[derive(Default)]
struct Signal {
    released: AtomicBool,
    wake: Notify,
}

static PARKS: LazyLock<Mutex<HashMap<PathBuf, Vec<Arc<Signal>>>>> = LazyLock::new(Mutex::default);

fn parks() -> std::sync::MutexGuard<'static, HashMap<PathBuf, Vec<Arc<Signal>>>> {
    PARKS.lock().unwrap_or_else(PoisonError::into_inner)
}

/// The key two spellings of one file share; a path that cannot be resolved keys as written.
pub(crate) fn file_key(path: &Path) -> PathBuf {
    std::fs::canonicalize(path).unwrap_or_else(|_| path.to_owned())
}

/// A read that may park on `file`, registered until dropped.
pub(crate) struct Parked {
    file: PathBuf,
    signal: Arc<Signal>,
}

impl Parked {
    /// Registers a read on `file`, a key from [`file_key`].
    pub(crate) fn hold(file: PathBuf) -> Parked {
        let signal = Arc::new(Signal::default());
        parks()
            .entry(file.clone())
            .or_default()
            .push(signal.clone());
        Parked { file, signal }
    }

    /// Whether a commit has asked this read to let go.
    pub(crate) fn is_released(&self) -> bool {
        self.signal.released.load(Ordering::SeqCst)
    }

    /// Resolves once a commit asks this read to let go; at once if one already has.
    pub(crate) async fn released(&self) {
        // The permit `notify_one` stores covers a release that lands between check and wait.
        while !self.is_released() {
            self.signal.wake.notified().await;
        }
    }
}

impl Drop for Parked {
    fn drop(&mut self) {
        let mut parks = parks();
        if let Some(list) = parks.get_mut(&self.file) {
            list.retain(|s| !Arc::ptr_eq(s, &self.signal));
            if list.is_empty() {
                parks.remove(&self.file);
            }
        }
    }
}

/// Asks every read registered on `file` to let go, then waits up to `wait` for them all to
/// end. Returns how many still hold on.
pub(crate) async fn release(file: &Path, wait: Duration) -> usize {
    let holding = || parks().get(file).map_or(0, Vec::len);
    for signal in parks().get(file).into_iter().flatten() {
        signal.released.store(true, Ordering::SeqCst);
        signal.wake.notify_one();
    }
    let deadline = tokio::time::Instant::now() + wait;
    while holding() > 0 && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    holding()
}
