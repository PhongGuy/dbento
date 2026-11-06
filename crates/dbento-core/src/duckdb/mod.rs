//! The DuckDB engine: one shared instance per database file, served to sessions as leases.
//!
//! Each session's lease lives on its own worker thread (`worker.rs`); the async half here
//! talks to it over the SQLite driver's channels.

mod cancel;
pub(crate) mod catalog;
mod classify;
mod registry;
mod render;
mod worker;

use std::sync::atomic::Ordering;
use std::sync::{Arc, mpsc as std_mpsc};
use std::time::Duration;

use ::duckdb::Connection;
use tokio::sync::{mpsc, oneshot};

pub(crate) use cancel::DuckCancel;
use cancel::WorkerGuard;
use registry::Mode;
use worker::{Ctl, Event, Job, Request, Shared};

use crate::introspect::{ColumnMeta, IntrospectError, RelationList, RelationRef, SchemaInfo};
use crate::session::{
    CancelHandle, ConnectError, ConnectParams, DRAIN_CAP, Driver, ExecuteError, ExtendError,
    Limits, OnTruncate, Prepared, StreamItem,
};
use crate::value::ColumnInfo;

/// The async handle to the worker thread that owns one session's lease.
pub(crate) struct DuckDriver {
    requests: std_mpsc::Sender<Request>,
    shared: Arc<Shared>,
    cancel: Arc<DuckCancel>,
    timeout: Option<Duration>,
    /// `current_database()` right after the open; None if it could not be read.
    opened_on: Option<String>,
}

/// The async handle to one script on the worker.
pub(crate) struct DuckScript {
    ctl: std_mpsc::Sender<Ctl>,
    cancel: Arc<DuckCancel>,
    timeout: Option<Duration>,
}

/// How long `settle` and `close` wait, past `DRAIN_CAP`, for a cancelled statement to stop.
const INTERRUPT_WAIT: Duration = Duration::from_secs(1);

/// One statement's events, plus the resume sender a parked worker waits on.
pub(crate) struct DuckStream {
    events: mpsc::Receiver<Event>,
    resume: Option<std_mpsc::Sender<Option<u64>>>,
    parked: bool,
}

impl DuckDriver {
    /// Whether a statement that changes session settings (SET, RESET, USE) has run.
    pub(crate) fn settings_changed(&self) -> bool {
        self.shared.settings_changed.load(Ordering::SeqCst)
    }

    /// Answered once every earlier script has let go; past `DRAIN_CAP` the running statement
    /// is cancelled, and the session closes if it has not stopped within `INTERRUPT_WAIT`.
    async fn settle_worker(&mut self) -> Result<(), ExecuteError> {
        let (done, wait) = oneshot::channel();
        if self.requests.send(Request::Settle { done }).is_err() {
            return Err(ExecuteError::SessionClosed);
        }
        if self.answered(wait).await {
            return Ok(());
        }
        let (done, _) = oneshot::channel();
        let _ = self.requests.send(Request::Close { done });
        Err(ExecuteError::SessionClosed)
    }

    /// Runs `read` on the worker behind every earlier script, as a statement of its own
    /// generation: bounded by the session's timeout, stopped by its cancel, and its errors
    /// mapped as a statement's are.
    pub(crate) async fn introspect<T: Send + 'static>(
        &mut self,
        read: impl FnOnce(&Connection) -> ::duckdb::Result<T> + Send + 'static,
    ) -> Result<T, IntrospectError> {
        // An earlier statement's drain error is that statement's; only a lost session counts.
        if let Err(e) = self.settle_worker().await
            && e.closes_session()
        {
            return Err(ExecuteError::SessionClosed.into());
        }
        let generation = self.cancel.next_generation();
        let (cancel, timeout, read_only) = (
            Arc::clone(&self.cancel),
            self.timeout,
            self.shared.read_only,
        );
        let (reply, answer) = oneshot::channel();
        let job: Job = Box::new(move |conn: Option<&Connection>| {
            let result = match conn {
                Some(c) => worker::run_read(c, &cancel, generation, timeout, read_only, read),
                None => Err(ExecuteError::SessionClosed),
            };
            let _ = reply.send(result);
        });
        if self.requests.send(Request::Introspect(job)).is_err() {
            return Err(ExecuteError::SessionClosed.into());
        }
        Ok(answer.await.unwrap_or(Err(ExecuteError::SessionClosed))?)
    }

    /// `current_database()` once a USE moved the session off the one it opened on.
    pub(crate) async fn moved_database(&mut self) -> Option<String> {
        let now = self.current().await?;
        (Some(&now) != self.opened_on.as_ref()).then_some(now)
    }

    async fn current(&mut self) -> Option<String> {
        self.introspect(|c| c.query_row("SELECT current_database()", [], |r| r.get(0)))
            .await
            .ok()
    }

    /// Whether the worker answers within `DRAIN_CAP`, or within `INTERRUPT_WAIT` of the cancel
    /// that follows it.
    async fn answered(&self, mut wait: oneshot::Receiver<()>) -> bool {
        match tokio::time::timeout(DRAIN_CAP, &mut wait).await {
            Ok(r) => r.is_ok(),
            Err(_) => {
                self.cancel.cancel();
                matches!(tokio::time::timeout(INTERRUPT_WAIT, wait).await, Ok(Ok(())))
            }
        }
    }
}

impl Driver for DuckDriver {
    type Script = DuckScript;
    type Stream = DuckStream;

    async fn connect(params: ConnectParams<'_>) -> Result<Self, ConnectError> {
        let ConnectParams {
            conn,
            read_only,
            timeout,
            trusted_sql,
            ..
        } = params;
        let path = conn
            .file_path()
            .ok_or_else(|| ConnectError::Open {
                message: "the connection has no database path".to_owned(),
            })?
            .to_owned();
        // Untrusted SQL could reach files or settings, so only the instance lock holds it; SQL
        // Dbento wrote shares a writer's instance, each statement in a read-only transaction.
        let mode = if conn.read_only() || (read_only && !trusted_sql) {
            Mode::Locked
        } else {
            Mode::ReadWrite
        };
        let shared = Arc::new(Shared::new(read_only, mode == Mode::ReadWrite));
        let owned = Arc::clone(&shared);
        let (requests, rx) = std_mpsc::channel();
        let (opened_tx, opened_rx) = oneshot::channel();
        let cancel =
            DuckCancel::start(Duration::from_millis(10)).map_err(|e| ConnectError::Open {
                message: e.to_string(),
            })?;
        let watch = WorkerGuard::new(Arc::clone(&cancel));
        // The open runs on the worker so the lease never crosses threads. The watchdog ends
        // with the worker, or with the closure when the thread cannot be spawned.
        std::thread::Builder::new()
            .name("dbento-duckdb".into())
            .spawn(move || match registry::open(&path, mode) {
                Ok(lease) => {
                    if opened_tx.send(Ok(())).is_ok() {
                        worker::serve(lease, &owned, &watch, rx);
                    }
                }
                Err(e) => drop(opened_tx.send(Err(e))),
            })
            .map_err(|e| ConnectError::Open {
                message: e.to_string(),
            })?;
        opened_rx.await.map_err(|_| ConnectError::Open {
            message: "the DuckDB worker stopped while opening".to_owned(),
        })??;
        let mut driver = DuckDriver {
            requests,
            shared,
            cancel,
            timeout,
            opened_on: None,
        };
        driver.opened_on = driver.current().await;
        Ok(driver)
    }

    fn start_script(&mut self, sql: &str) -> DuckScript {
        let (ctl, ctl_rx) = std_mpsc::channel();
        // A dead worker drops the receiver; the first `Next` then reports the session closed.
        let _ = self.requests.send(Request::Script {
            sql: sql.to_owned(),
            ctl: ctl_rx,
        });
        DuckScript {
            ctl,
            cancel: Arc::clone(&self.cancel),
            timeout: self.timeout,
        }
    }

    async fn prepare_next(script: &mut DuckScript) -> Option<Result<Prepared, ExecuteError>> {
        let (reply, wait) = oneshot::channel();
        if script.ctl.send(Ctl::Next { reply }).is_err() {
            return Some(Err(ExecuteError::SessionClosed));
        }
        wait.await.unwrap_or(Some(Err(ExecuteError::SessionClosed)))
    }

    async fn is_last(script: &mut DuckScript) -> bool {
        let (reply, wait) = oneshot::channel();
        if script.ctl.send(Ctl::IsLast { reply }).is_err() {
            return true;
        }
        wait.await.unwrap_or(true)
    }

    async fn execute(
        script: &mut DuckScript,
        limits: Limits,
    ) -> Result<(Vec<ColumnInfo>, DuckStream), ExecuteError> {
        let (reply, wait) = oneshot::channel();
        let (events, rx) = mpsc::channel(1);
        let (resume, resume_rx) = match limits.on_truncate {
            OnTruncate::Park => {
                let (tx, rx) = std_mpsc::channel();
                (Some(tx), Some(rx))
            }
            _ => (None, None),
        };
        // Taken before the request goes out, so a cancel from now on aims at this statement.
        let generation = script.cancel.next_generation();
        if script
            .ctl
            .send(Ctl::Execute {
                reply,
                events,
                limits,
                resume: resume_rx,
                generation,
                timeout: script.timeout,
            })
            .is_err()
        {
            return Err(ExecuteError::SessionClosed);
        }
        let columns = wait.await.map_err(|_| ExecuteError::SessionClosed)??;
        let stream = DuckStream {
            events: rx,
            resume,
            parked: false,
        };
        Ok((columns, stream))
    }

    async fn next_event(stream: &mut DuckStream) -> Result<StreamItem, ExecuteError> {
        match stream.events.recv().await {
            Some(Event::Batch(b)) => Ok(StreamItem::Batch(b)),
            Some(Event::Done(o)) => Ok(StreamItem::Done(o)),
            Some(Event::Parked(o)) => {
                stream.parked = true;
                Ok(StreamItem::Done(o))
            }
            Some(Event::Failed(e)) => Err(e),
            None => Err(ExecuteError::SessionClosed),
        }
    }

    fn extend(stream: &mut DuckStream, more: Option<u64>) -> Result<(), ExtendError> {
        if !std::mem::take(&mut stream.parked) {
            return Err(ExtendError::NotParked);
        }
        let resume = stream.resume.as_ref().ok_or(ExtendError::NotParked)?;
        resume.send(more).map_err(|_| ExtendError::SessionClosed)
    }

    fn cancel_handle(&self) -> CancelHandle {
        CancelHandle::duckdb(Arc::clone(&self.cancel))
    }

    /// Settles the worker. A dropped DuckDB stream leaves no failure behind: its write was
    /// applied at execute.
    async fn settle(&mut self) -> Result<(), ExecuteError> {
        self.settle_worker().await
    }

    /// Returns once the worker has dropped the lease.
    async fn close(self) -> Result<(), ExecuteError> {
        let (done, wait) = oneshot::channel();
        if self.requests.send(Request::Close { done }).is_ok() && !self.answered(wait).await {
            return Err(ExecuteError::SessionClosed);
        }
        Ok(())
    }

    async fn schemas(&mut self) -> Result<Vec<SchemaInfo>, IntrospectError> {
        self.introspect(catalog::schemas).await
    }

    async fn relations(&mut self, schema: &str) -> Result<RelationList, IntrospectError> {
        let schema = schema.to_owned();
        self.introspect(move |c| catalog::relations(c, &schema))
            .await
    }

    async fn columns(&mut self, rel: &RelationRef) -> Result<Vec<ColumnMeta>, IntrospectError> {
        let (schema, name) = (rel.schema().to_owned(), rel.name().to_owned());
        self.introspect(move |c| catalog::columns(c, &schema, &name))
            .await
    }
}
