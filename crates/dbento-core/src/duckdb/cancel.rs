//! The per-session stop guard: it marks the running DuckDB statement cancelled or timed out
//! and fires the connection's interrupt from a watchdog thread.
//!
//! DuckDB clears a pending interrupt whenever a prepare or an execution starts, so until the
//! worker reports `executed` the watchdog re-fires every `refire`; after that one fire holds.

use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use crate::session::ExecuteError;

#[cfg(test)]
mod tests;

/// Fires DuckDB's interrupt on the session's connection.
pub(crate) type Interrupt = Arc<dyn Fn() + Send + Sync>;

/// Why a statement was stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stop {
    Cancelled,
    Timeout,
}

impl From<Stop> for ExecuteError {
    fn from(stop: Stop) -> Self {
        match stop {
            Stop::Cancelled => ExecuteError::Cancelled,
            Stop::Timeout => ExecuteError::Timeout,
        }
    }
}

/// The statement between `begin` and `end`.
struct Running {
    generation: u64,
    deadline: Option<Instant>,
    interrupt: Interrupt,
    stop: Option<Stop>,
    executed: bool,
}

struct State {
    /// The latest generation handed out; the first is 1.
    generation: u64,
    /// The latest generation a cancel aimed at; 0 for none.
    cancelled: u64,
    running: Option<Running>,
    shut: bool,
}

/// Cancellation and timeout for one DuckDB session. Each statement takes a new generation,
/// as `SqliteCancel` does, so a cancel can only ever reach the statement it aimed at.
pub(crate) struct DuckCancel {
    state: Mutex<State>,
    wake: Condvar,
    refire: Duration,
}

impl std::fmt::Debug for DuckCancel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let st = self.lock();
        f.debug_struct("DuckCancel")
            .field("generation", &st.generation)
            .field("cancelled", &st.cancelled)
            .field(
                "running",
                &st.running.as_ref().map(|r| (r.generation, r.stop)),
            )
            .finish_non_exhaustive()
    }
}

impl DuckCancel {
    /// Starts the guard and its watchdog thread, which runs until `shut`.
    pub(crate) fn start(refire: Duration) -> std::io::Result<Arc<DuckCancel>> {
        let guard = Arc::new(DuckCancel {
            state: Mutex::new(State {
                generation: 0,
                cancelled: 0,
                running: None,
                shut: false,
            }),
            wake: Condvar::new(),
            refire,
        });
        let watched = Arc::clone(&guard);
        std::thread::Builder::new()
            .name("dbento-duckdb-watch".into())
            .spawn(move || watched.watch())?;
        Ok(guard)
    }

    /// Takes the generation of the statement about to be sent to the worker.
    pub(crate) fn next_generation(&self) -> u64 {
        let mut st = self.lock();
        st.generation += 1;
        st.generation
    }

    /// Cancels the latest statement, whether it is running yet or not.
    pub(crate) fn cancel(&self) {
        let mut st = self.lock();
        let latest = st.generation;
        st.cancelled = st.cancelled.max(latest);
        if let Some(r) = st.running.as_mut()
            && r.generation == latest
            && r.stop.is_none()
        {
            r.stop = Some(Stop::Cancelled);
            (r.interrupt)();
            self.wake.notify_all();
        }
    }

    /// Marks `generation` running until `end`; a cancel that already reached it answers here.
    pub(crate) fn begin(
        &self,
        generation: u64,
        deadline: Option<Instant>,
        interrupt: Interrupt,
    ) -> Result<(), Stop> {
        let mut st = self.lock();
        if st.cancelled == generation {
            return Err(Stop::Cancelled);
        }
        st.running = Some(Running {
            generation,
            deadline,
            interrupt,
            stop: None,
            executed: false,
        });
        self.wake.notify_all();
        Ok(())
    }

    /// The statement's execution has started; its start cleared any earlier fire, so a stop
    /// already set fires once more.
    pub(crate) fn executed(&self) {
        let mut st = self.lock();
        if let Some(r) = st.running.as_mut() {
            r.executed = true;
            if r.stop.is_some() {
                (r.interrupt)();
            }
        }
        self.wake.notify_all();
    }

    /// The running statement's stop, if any.
    pub(crate) fn stopped(&self) -> Option<Stop> {
        self.lock().running.as_ref().and_then(|r| r.stop)
    }

    /// Ends the running statement and returns its stop; nothing fires after it returns.
    pub(crate) fn end(&self) -> Option<Stop> {
        let stop = self.lock().running.take().and_then(|r| r.stop);
        self.wake.notify_all();
        stop
    }

    /// Ends the watchdog thread.
    pub(crate) fn shut(&self) {
        self.lock().shut = true;
        self.wake.notify_all();
    }

    /// Each mutation is a single assignment, so a poisoned lock is still consistent.
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Fires the running statement's interrupt: at its deadline, and again every `refire`
    /// while it is stopped but its execution has not started. A stop already set is kept.
    fn watch(&self) {
        let mut st = self.lock();
        while !st.shut {
            let now = Instant::now();
            let wait = match st.running.as_mut() {
                Some(r) if r.stop.is_some() => {
                    if r.executed {
                        None
                    } else {
                        (r.interrupt)();
                        Some(self.refire)
                    }
                }
                Some(r) => match r.deadline {
                    Some(d) if d <= now => {
                        r.stop = Some(Stop::Timeout);
                        (r.interrupt)();
                        (!r.executed).then_some(self.refire)
                    }
                    Some(d) => Some(d - now),
                    None => None,
                },
                None => None,
            };
            st = match wait {
                Some(d) => {
                    self.wake
                        .wait_timeout(st, d)
                        .unwrap_or_else(PoisonError::into_inner)
                        .0
                }
                None => self.wake.wait(st).unwrap_or_else(PoisonError::into_inner),
            };
        }
    }
}

/// Held by the worker thread: however the thread ends, even by a panic, the running
/// statement ends and the watchdog stops, so nothing fires on a connection no one serves.
pub(crate) struct WorkerGuard(Arc<DuckCancel>);

impl WorkerGuard {
    pub(crate) fn new(cancel: Arc<DuckCancel>) -> Self {
        WorkerGuard(cancel)
    }
}

impl std::ops::Deref for WorkerGuard {
    type Target = DuckCancel;

    fn deref(&self) -> &DuckCancel {
        &self.0
    }
}

impl Drop for WorkerGuard {
    fn drop(&mut self) {
        self.0.end();
        self.0.shut();
    }
}
