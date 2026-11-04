//! The app's query history: entries queued on the main thread and written in order off it.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use dbento_core::{ExecuteError, History, HistoryEntry, HistoryFilter, HistoryOutcome};
use gpui_kit::{App, AppContext as _, Context, Task};

use crate::runner::{StatementEnd, StopReason};

/// The history file as the app uses it, opened lazily on the first write or read.
pub struct HistoryLog {
    path: PathBuf,
    history: Arc<Mutex<Option<History>>>,
    queue: Arc<Mutex<Vec<HistoryEntry>>>,
    written: u64,
}

impl HistoryLog {
    /// A log over the history file at `path`; nothing is opened yet.
    pub fn new(path: PathBuf) -> HistoryLog {
        HistoryLog {
            path,
            history: Arc::new(Mutex::new(None)),
            queue: Arc::new(Mutex::new(Vec::new())),
            written: 0,
        }
    }

    /// Queues `entries` and writes the queue in order off the main thread; notifies once
    /// something was written.
    pub fn append(&mut self, entries: Vec<HistoryEntry>, cx: &mut Context<Self>) {
        if entries.is_empty() {
            return;
        }
        lock(&self.queue).extend(entries);
        let (path, history, queue) = (self.path.clone(), self.history.clone(), self.queue.clone());
        cx.spawn(async move |this, cx| {
            let written = cx
                .background_spawn(async move { write_queue(&path, &history, &queue) })
                .await;
            let _ = this.update(cx, |log, cx| {
                if written > 0 {
                    log.written += written;
                    cx.notify();
                }
            });
        })
        .detach();
    }

    /// Up to `limit` entries matching `filter`, newest first, read off the main thread; none
    /// when the file cannot be opened.
    pub fn recent(&self, filter: HistoryFilter, limit: usize, cx: &App) -> Task<Vec<HistoryEntry>> {
        let (path, history) = (self.path.clone(), self.history.clone());
        cx.background_executor().spawn(async move {
            let mut slot = lock(&history);
            opened(&path, &mut slot).map_or_else(Vec::new, |h| h.recent(&filter, limit))
        })
    }

    /// How many entries this log has written so far.
    pub fn written(&self) -> u64 {
        self.written
    }
}

/// How a statement's end reads in the history.
pub fn history_outcome(end: &StatementEnd) -> HistoryOutcome {
    match end {
        StatementEnd::Complete(o) | StatementEnd::Truncated(o) => HistoryOutcome::Ok {
            rows: o.rows,
            truncated: o.truncated,
            rows_affected: o.rows_affected,
        },
        // Plan results are never recorded, so `Planned` does not reach here in practice.
        StatementEnd::Skipped | StatementEnd::Planned => HistoryOutcome::Skipped,
        StatementEnd::Stopped { rows, reason } => match reason {
            StopReason::User | StopReason::NewRun | StopReason::Closed => HistoryOutcome::Cancelled,
            StopReason::MemoryBudget => HistoryOutcome::Ok {
                rows: *rows,
                truncated: true,
                rows_affected: None,
            },
            StopReason::Lost => HistoryOutcome::Failed {
                message: "session stopped".into(),
            },
        },
        StatementEnd::Failed(ExecuteError::Cancelled) => HistoryOutcome::Cancelled,
        StatementEnd::Failed(e) => HistoryOutcome::Failed {
            message: e.to_string(),
        },
    }
}

// Takes the whole queue while holding the history lock, so concurrent writers keep append
// order; returns how many entries were written. Entries that cannot be written are dropped.
fn write_queue(
    path: &std::path::Path,
    history: &Mutex<Option<History>>,
    queue: &Mutex<Vec<HistoryEntry>>,
) -> u64 {
    let mut slot = lock(history);
    let entries = std::mem::take(&mut *lock(queue));
    let Some(h) = opened(path, &mut slot) else {
        return 0;
    };
    let mut written = 0;
    let mut warned = false;
    for entry in &entries {
        match h.append(entry) {
            Ok(()) => written += 1,
            Err(e) if !warned => {
                warned = true;
                eprintln!("dbento: cannot write the query history: {e}");
            }
            Err(_) => {}
        }
    }
    written
}

// The open history in `slot`, opening it first when needed; an open failure is logged.
fn opened<'a>(path: &std::path::Path, slot: &'a mut Option<History>) -> Option<&'a mut History> {
    if slot.is_none() {
        match History::open(path) {
            Ok(h) => *slot = Some(h),
            Err(e) => eprintln!("dbento: cannot write the query history: {e}"),
        }
    }
    slot.as_mut()
}

// A poisoned lock still guards plain data, so a panicked writer does not stop the log.
fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
}
