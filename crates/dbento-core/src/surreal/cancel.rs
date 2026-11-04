//! Stops a stream from any thread: `CancelHandle`, the statement deadline and Dbento's own cuts
//! all come here, and the stream's request id is the generation that keeps a late stop from
//! landing on the next statement.

use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use tokio::sync::Notify;

use super::rpc::Rpc;
use crate::session::ExecuteError;

/// Why Dbento stopped a stream.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stop {
    Cancelled,
    Timeout,
    /// A cut no caller needs to hear about.
    Quiet,
}

impl Stop {
    pub(crate) fn error(self) -> ExecuteError {
        match self {
            Stop::Timeout => ExecuteError::Timeout,
            Stop::Cancelled | Stop::Quiet => ExecuteError::Cancelled,
        }
    }
}

#[derive(Default)]
struct Target {
    handed: Option<i64>,
    stop: Option<(i64, Stop, Instant)>,
    watchdog: Option<tokio::task::AbortHandle>,
}

/// Stops the session's stream in flight. The stream's request id is its generation, so a stop
/// that comes late names a stream already over and the server refuses it.
pub(crate) struct SurrealCancel {
    rpc: Rpc,
    target: Mutex<Target>,
    stopped: Notify,
    bound: Duration,
}

impl std::fmt::Debug for SurrealCancel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SurrealCancel").finish_non_exhaustive()
    }
}

impl SurrealCancel {
    pub(crate) fn new(rpc: Rpc, bound: Duration) -> Self {
        SurrealCancel {
            rpc,
            target: Mutex::default(),
            stopped: Notify::new(),
            bound,
        }
    }

    fn lock(&self) -> MutexGuard<'_, Target> {
        self.target.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Stops the stream handed out, if any; never blocks.
    pub(crate) fn cancel(&self) {
        let handed = self.lock().handed;
        if let Some(id) = handed {
            self.stop(id, Stop::Cancelled);
        }
    }

    pub(crate) fn stop(&self, id: i64, why: Stop) {
        {
            let mut t = self.lock();
            if t.handed != Some(id) || t.stop.is_some_and(|(s, ..)| s == id) {
                return;
            }
            t.stop = Some((id, why, Instant::now()));
        }
        self.rpc.query_cancel(id);
        self.stopped.notify_waiters();
    }

    pub(crate) fn begin(&self, id: i64) {
        let mut t = self.lock();
        Self::idle(&mut t);
        t.handed = Some(id);
    }

    pub(crate) fn end(&self) {
        Self::idle(&mut self.lock());
    }

    fn idle(t: &mut Target) {
        t.handed = None;
        if let Some(watchdog) = t.watchdog.take() {
            watchdog.abort();
        }
    }

    pub(crate) fn arm(self: &Arc<Self>, id: i64, after: Duration) {
        let this = Arc::clone(self);
        let task = tokio::spawn(async move {
            tokio::time::sleep(after).await;
            this.stop(id, Stop::Timeout);
        });
        if let Some(old) = self.lock().watchdog.replace(task.abort_handle()) {
            old.abort();
        }
    }

    pub(crate) fn stopped(&self, id: i64) -> Option<Stop> {
        self.lock()
            .stop
            .filter(|&(s, ..)| s == id)
            .map(|(_, why, _)| why)
    }

    // Resolves the drain bound after stream `id` was stopped, for a server that never answers.
    pub(crate) async fn past_bound(&self, id: i64) {
        let at = loop {
            let mut woken = std::pin::pin!(self.stopped.notified());
            woken.as_mut().enable();
            if let Some((_, _, at)) = self.lock().stop.filter(|&(s, ..)| s == id) {
                break at;
            }
            woken.await;
        };
        tokio::time::sleep_until(tokio::time::Instant::from_std(at + self.bound)).await;
    }

    pub(crate) async fn until_stopped(&self, id: i64) {
        loop {
            let mut woken = std::pin::pin!(self.stopped.notified());
            woken.as_mut().enable();
            if self.stopped(id).is_some() {
                return;
            }
            woken.await;
        }
    }
}
