//! The SQLite driver: the connection lives on a dedicated worker thread.
//!
//! Async callers talk to it over channels: one request channel per session, one control
//! channel per script, and one depth-1 event channel per statement, whose backpressure
//! keeps at most one batch in flight.

use std::fmt::Write as _;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError, mpsc as std_mpsc};
use std::time::{Duration, Instant};

use rusqlite::fallible_iterator::FallibleIterator as _;
use rusqlite::limits::Limit;
use rusqlite::types::{Value, ValueRef};
use rusqlite::{Batch, Connection, ErrorCode, OpenFlags, Row, Statement, params_from_iter};
use tokio::sync::{mpsc, oneshot};

use crate::classify::{counts_rows, sqlite_classification};
use crate::introspect::{
    ColumnMeta, IntrospectError, RELATION_CAP, Relation, RelationKind, RelationList, RelationRef,
    SchemaInfo, capped, quote_ident,
};
use crate::ranges::statement_ranges;
use crate::rowedit::{EditValue, Param};
use crate::session::{
    CancelHandle, ConnectError, ConnectParams, DRAIN_CAP, Driver, ExecuteError, ExtendError,
    Limits, OnTruncate, Outcome, Prepared, StreamItem, TruncatedBy,
};
use crate::value::{BatchBuilder, CellRef, ColumnInfo, ColumnSource, ResultBatch};
use crate::workspace::Engine;

enum Request {
    Script {
        sql: String,
        ctl: std_mpsc::Receiver<Ctl>,
    },
    /// Answered once every earlier script has let go of its statements.
    Settle {
        done: oneshot::Sender<()>,
    },
    Close {
        done: oneshot::Sender<()>,
    },
    /// A catalog read; it gets `None` once the connection is closed and answers itself.
    Introspect(Job),
}

/// A catalog read packaged with its own reply channel.
type Job = Box<dyn FnOnce(Option<&Connection>) + Send>;

/// How long `settle` waits, past `DRAIN_CAP`, for a cancelled statement to stop.
const INTERRUPT_WAIT: Duration = Duration::from_secs(1);

enum Ctl {
    Next {
        reply: oneshot::Sender<Option<Result<Prepared, ExecuteError>>>,
    },
    IsLast {
        reply: oneshot::Sender<bool>,
    },
    Execute {
        reply: oneshot::Sender<Result<Vec<ColumnInfo>, ExecuteError>>,
        events: mpsc::Sender<Event>,
        limits: Limits,
        timeout: Option<Duration>,
        generation: u64,
        /// Present under `OnTruncate::Park`: the worker waits on it at a row cut.
        resume: Option<std_mpsc::Receiver<Option<u64>>>,
    },
}

impl Ctl {
    fn reject(self, e: ExecuteError) {
        match self {
            Ctl::Next { reply } => drop(reply.send(Some(Err(e)))),
            // A closed session runs nothing more, so nothing follows.
            Ctl::IsLast { reply } => drop(reply.send(true)),
            Ctl::Execute { reply, .. } => drop(reply.send(Err(e))),
        }
    }
}

pub(crate) enum Event {
    Batch(ResultBatch),
    Done(Outcome),
    /// A row cut under `OnTruncate::Park`: the worker holds the statement until resumed.
    Parked(Outcome),
    Failed(ExecuteError),
}

/// One statement's events, plus the resume sender a parked worker waits on. Dropping the
/// stream drops the sender, which wakes a parked worker so it finishes a write or resets a read.
pub(crate) struct SqliteStream {
    events: mpsc::Receiver<Event>,
    resume: Option<std_mpsc::Sender<Option<u64>>>,
    parked: bool,
}

/// Cancellation shared by the session's handles and its worker. Each `execute` takes a new
/// generation; a cancel marks the latest one, so it can only ever hit that statement.
/// Only the statement's progress handler acts on the mark: `sqlite3_interrupt` would also
/// hit whatever the worker prepares or runs next.
#[derive(Debug)]
pub(crate) struct SqliteCancel {
    generation: AtomicU64,
    cancelled: AtomicU64,
}

impl SqliteCancel {
    pub(crate) fn cancel(&self) {
        // fetch_max keeps a late-running older cancel from overwriting a newer mark.
        self.cancelled
            .fetch_max(self.generation.load(Ordering::SeqCst), Ordering::SeqCst);
    }

    fn hits(&self, generation: u64) -> bool {
        self.cancelled.load(Ordering::SeqCst) == generation
    }
}

/// A dropped write's failed commit that no caller has reported; `settle` or `close` does.
type Unreported = Arc<Mutex<Option<ExecuteError>>>;

/// The running statement's generation and deadline, which the connection's one progress
/// handler polls. Empty between statements, so nothing else is ever interrupted.
type Armed = Arc<Mutex<Option<(u64, Option<Instant>)>>>;

/// Takes the dropped write's failed commit no caller has reported yet.
fn take_unreported(unreported: &Unreported) -> Option<ExecuteError> {
    unreported
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take()
}

/// The async handle to a worker thread that owns one SQLite connection.
pub(crate) struct SqliteDriver {
    requests: std_mpsc::Sender<Request>,
    timeout: Option<Duration>,
    cancel: Arc<SqliteCancel>,
    unreported: Unreported,
}

/// The async handle to one script on the worker.
pub(crate) struct SqliteScript {
    ctl: std_mpsc::Sender<Ctl>,
    timeout: Option<Duration>,
    cancel: Arc<SqliteCancel>,
}

impl Driver for SqliteDriver {
    type Script = SqliteScript;
    type Stream = SqliteStream;

    async fn connect(params: ConnectParams<'_>) -> Result<Self, ConnectError> {
        let ConnectParams {
            conn,
            read_only,
            timeout,
            ..
        } = params;
        let path = conn.sqlite_path().ok_or_else(|| ConnectError::Open {
            message: "the connection has no database path".to_owned(),
        })?;
        // The open runs on the worker so the connection never crosses threads.
        let path = path.to_owned();
        let unreported = Unreported::default();
        let owed = Arc::clone(&unreported);
        let (requests, rx) = std_mpsc::channel();
        let (opened_tx, opened_rx) = oneshot::channel();
        std::thread::Builder::new()
            .name("dbento-sqlite".into())
            .spawn(move || match open(&path, read_only) {
                Ok(conn) => {
                    let cancel = Arc::new(SqliteCancel {
                        generation: AtomicU64::new(0),
                        cancelled: AtomicU64::new(0),
                    });
                    if opened_tx.send(Ok(Arc::clone(&cancel))).is_ok() {
                        worker(conn, read_only, &cancel, &owed, rx);
                    }
                }
                Err(e) => drop(opened_tx.send(Err(e))),
            })
            .map_err(|e| ConnectError::Open {
                message: e.to_string(),
            })?;
        let cancel = opened_rx.await.map_err(|_| ConnectError::Open {
            message: "the SQLite worker stopped while opening".to_owned(),
        })??;
        Ok(SqliteDriver {
            requests,
            timeout,
            cancel,
            unreported,
        })
    }

    fn start_script(&mut self, sql: &str) -> SqliteScript {
        let (ctl, ctl_rx) = std_mpsc::channel();
        // A dead worker drops the receiver; the first `Next` then reports the session closed.
        let _ = self.requests.send(Request::Script {
            sql: sql.to_owned(),
            ctl: ctl_rx,
        });
        SqliteScript {
            ctl,
            timeout: self.timeout,
            cancel: Arc::clone(&self.cancel),
        }
    }

    async fn prepare_next(script: &mut SqliteScript) -> Option<Result<Prepared, ExecuteError>> {
        let (reply, wait) = oneshot::channel();
        if script.ctl.send(Ctl::Next { reply }).is_err() {
            return Some(Err(ExecuteError::SessionClosed));
        }
        wait.await.unwrap_or(Some(Err(ExecuteError::SessionClosed)))
    }

    async fn is_last(script: &mut SqliteScript) -> bool {
        let (reply, wait) = oneshot::channel();
        if script.ctl.send(Ctl::IsLast { reply }).is_err() {
            return true;
        }
        wait.await.unwrap_or(true)
    }

    async fn execute(
        script: &mut SqliteScript,
        limits: Limits,
    ) -> Result<(Vec<ColumnInfo>, Self::Stream), ExecuteError> {
        let (reply, wait) = oneshot::channel();
        let (events, rx) = mpsc::channel(1);
        // Generations start at 1 because `cancelled` starts at 0, which no statement owns.
        let generation = script.cancel.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let (resume, resume_rx) = match limits.on_truncate {
            OnTruncate::Park => {
                let (tx, rx) = std_mpsc::channel();
                (Some(tx), Some(rx))
            }
            _ => (None, None),
        };
        if script
            .ctl
            .send(Ctl::Execute {
                reply,
                events,
                limits,
                timeout: script.timeout,
                generation,
                resume: resume_rx,
            })
            .is_err()
        {
            return Err(ExecuteError::SessionClosed);
        }
        let columns = wait.await.map_err(|_| ExecuteError::SessionClosed)??;
        let stream = SqliteStream {
            events: rx,
            resume,
            parked: false,
        };
        Ok((columns, stream))
    }

    async fn next_event(stream: &mut Self::Stream) -> Result<StreamItem, ExecuteError> {
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

    fn extend(stream: &mut Self::Stream, more: Option<u64>) -> Result<(), ExtendError> {
        if !std::mem::take(&mut stream.parked) {
            return Err(ExtendError::NotParked);
        }
        let resume = stream.resume.as_ref().ok_or(ExtendError::NotParked)?;
        resume.send(more).map_err(|_| ExtendError::SessionClosed)
    }

    fn cancel_handle(&self) -> CancelHandle {
        CancelHandle::sqlite(Arc::clone(&self.cancel))
    }

    /// Settles the worker, then reports a dropped write's failed commit once.
    async fn settle(&mut self) -> Result<(), ExecuteError> {
        self.settle_worker().await?;
        take_unreported(&self.unreported).map_or(Ok(()), Err)
    }

    /// Closes the connection once a dropped write has stepped to its end, returning its
    /// failed commit; past the cap the write's fate is unknown, so the session reads closed.
    async fn close(self) -> Result<(), ExecuteError> {
        let (done, wait) = oneshot::channel();
        if self.requests.send(Request::Close { done }).is_ok() && !self.answered(wait).await {
            return Err(ExecuteError::SessionClosed);
        }
        take_unreported(&self.unreported).map_or(Ok(()), Err)
    }

    async fn schemas(&mut self) -> Result<Vec<SchemaInfo>, IntrospectError> {
        self.introspect(|c| {
            c.prepare(SCHEMAS_SQL)?
                .query_map([], |r| Ok(SchemaInfo::new(r.get::<_, String>(0)?)))?
                .collect()
        })
        .await
    }

    async fn relations(&mut self, schema: &str) -> Result<RelationList, IntrospectError> {
        let schema = schema.to_owned();
        self.introspect(move |c| {
            // The schema cannot be bound, so an unknown one is answered before it is quoted in.
            if !known_schema(c, &schema)? {
                return Ok(RelationList::new(Vec::new(), false));
            }
            let sql = format!(
                "SELECT name, type FROM {}.sqlite_schema \
                 WHERE type IN ('table', 'view') AND name NOT LIKE 'sqlite\\_%' ESCAPE '\\' \
                 ORDER BY name LIMIT ?1",
                quote_ident(Engine::Sqlite, &schema)
            );
            let limit = i64::try_from(RELATION_CAP + 1).unwrap_or(i64::MAX);
            let items = c
                .prepare(&sql)?
                .query_map([limit], |r| {
                    let kind = match r.get_ref(1)?.as_str()? {
                        "view" => RelationKind::View,
                        _ => RelationKind::Table,
                    };
                    Ok(Relation::new(r.get::<_, String>(0)?, kind))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(capped(items))
        })
        .await
    }

    async fn columns(&mut self, rel: &RelationRef) -> Result<Vec<ColumnMeta>, IntrospectError> {
        let (schema, name) = (rel.schema().to_owned(), rel.name().to_owned());
        self.introspect(move |c| {
            // pragma_table_xinfo errors on an unknown schema, where the contract wants empty.
            if !known_schema(c, &schema)? {
                return Ok(Vec::new());
            }
            c.prepare(COLUMNS_SQL)?
                .query_map([&name, &schema], |r| {
                    // Never out of range: SQLite caps a table at 32767 columns.
                    let pk = u16::try_from(r.get::<_, i64>(4)?).ok().filter(|&p| p > 0);
                    Ok(ColumnMeta::new(
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        !r.get::<_, bool>(2)?,
                        pk,
                        r.get(3)?,
                    ))
                })?
                .collect()
        })
        .await
    }
}

/// Whether `schema` is `main` or an attached database; `temp` never counts.
pub(crate) fn known_schema(c: &Connection, schema: &str) -> rusqlite::Result<bool> {
    c.query_row(
        "SELECT EXISTS (SELECT 1 FROM pragma_database_list WHERE name = ?1 AND name <> 'temp')",
        [schema],
        |r| r.get(0),
    )
}

const SCHEMAS_SQL: &str = "SELECT name FROM pragma_database_list WHERE name <> 'temp' ORDER BY seq";

/// Hidden virtual-table columns (`hidden = 1`) stay out; generated ones (2, 3) stay in.
const COLUMNS_SQL: &str = "SELECT name, type, \"notnull\", dflt_value, pk \
     FROM pragma_table_xinfo(?1, ?2) WHERE hidden <> 1 ORDER BY cid";

impl SqliteDriver {
    /// Only a dropped stream's statement can still be owed, since truncation finishes inline.
    /// The worker answers once it lets go; past the cap that statement is cancelled, and if
    /// it still holds on, the session is closed behind it. A dropped write's failed commit
    /// stays unreported for `Driver::settle` or `close`.
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

    /// Waits for the worker to answer `wait`, cancelling its statement past `DRAIN_CAP`.
    async fn answered(&self, mut wait: oneshot::Receiver<()>) -> bool {
        match tokio::time::timeout(DRAIN_CAP, &mut wait).await {
            Ok(r) => r.is_ok(),
            Err(_) => {
                self.cancel.cancel();
                matches!(tokio::time::timeout(INTERRUPT_WAIT, wait).await, Ok(Ok(())))
            }
        }
    }

    /// Runs `read` on the worker behind every earlier script, once they have settled.
    pub(crate) async fn introspect<T: Send + 'static>(
        &mut self,
        read: impl FnOnce(&Connection) -> rusqlite::Result<T> + Send + 'static,
    ) -> Result<T, IntrospectError> {
        Ok(self.on_worker(read).await?)
    }

    /// Runs one statement with text and blob parameters and returns the rows it changed.
    pub(crate) async fn run_bound(
        &mut self,
        sql: &str,
        params: &[Param],
    ) -> Result<u64, ExecuteError> {
        let sql = sql.to_owned();
        let values: Vec<Value> = params
            .iter()
            .map(|p| match p {
                Param::Text(t) => Value::Text(t.clone()),
                Param::Bytes(b) => Value::Blob(b.clone()),
            })
            .collect();
        let changed = self
            .on_worker(move |c| c.execute(&sql, params_from_iter(values)))
            .await?;
        Ok(changed as u64)
    }

    /// Reads every `EXPLAIN QUERY PLAN` row as `(id, parent, notused, detail)`, once earlier
    /// scripts have settled; the statement itself is only planned, never run.
    pub(crate) async fn explain(
        &mut self,
        sql: &str,
    ) -> Result<Vec<(i64, i64, i64, String)>, ExecuteError> {
        let sql = sql.to_owned();
        self.on_worker(move |c| {
            let mut statement = c.prepare(&sql)?;
            let rows =
                statement.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
            rows.collect()
        })
        .await
    }

    /// Runs a bound statement and returns its rows, each value rendered as a browse renders it.
    pub(crate) async fn query_bound(
        &mut self,
        sql: &str,
        params: &[Param],
    ) -> Result<Vec<Vec<EditValue>>, ExecuteError> {
        let sql = sql.to_owned();
        let values: Vec<Value> = params
            .iter()
            .map(|p| match p {
                Param::Text(t) => Value::Text(t.clone()),
                Param::Bytes(b) => Value::Blob(b.clone()),
            })
            .collect();
        self.on_worker(move |c| {
            let mut statement = c.prepare(&sql)?;
            let width = statement.column_count();
            let mut rows = statement.query(params_from_iter(values))?;
            let mut out = Vec::new();
            let mut scratch = String::new();
            while let Some(row) = rows.next()? {
                let values = (0..width)
                    .map(|i| Ok(owned(rendered(row.get_ref(i)?, &mut scratch))))
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                out.push(values);
            }
            Ok(out)
        })
        .await
    }

    /// Runs `job` on the worker's connection once every earlier script has settled.
    async fn on_worker<T: Send + 'static>(
        &mut self,
        job: impl FnOnce(&Connection) -> rusqlite::Result<T> + Send + 'static,
    ) -> Result<T, ExecuteError> {
        // An earlier statement's drain error is that statement's; only a lost session counts.
        if let Err(e) = self.settle_worker().await
            && e.closes_session()
        {
            return Err(ExecuteError::SessionClosed);
        }
        let (reply, answer) = oneshot::channel();
        let job = Box::new(move |conn: Option<&Connection>| {
            let result = match conn {
                Some(c) => job(c).map_err(map_error),
                None => Err(ExecuteError::SessionClosed),
            };
            let _ = reply.send(result);
        });
        if self.requests.send(Request::Introspect(job)).is_err() {
            return Err(ExecuteError::SessionClosed);
        }
        answer.await.unwrap_or(Err(ExecuteError::SessionClosed))
    }
}

/// Opens an existing file only; SQLite would otherwise create an empty database.
/// A read-only session also refuses ATTACH, so no second database can be opened writable.
fn open(path: &Path, read_only: bool) -> Result<Connection, ConnectError> {
    if !path.exists() {
        return Err(ConnectError::NotFound {
            path: path.to_owned(),
        });
    }
    let mode = if read_only {
        OpenFlags::SQLITE_OPEN_READ_ONLY
    } else {
        OpenFlags::SQLITE_OPEN_READ_WRITE
    };
    let flags = mode | OpenFlags::SQLITE_OPEN_URI | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let open_error = |e: rusqlite::Error| ConnectError::Open {
        message: e.to_string(),
    };
    let conn = Connection::open_with_flags(path, flags).map_err(open_error)?;
    if read_only {
        conn.set_limit(Limit::SQLITE_LIMIT_ATTACHED, 0)
            .map_err(open_error)?;
    }
    Ok(conn)
}

/// Serves requests until the session drops its sender. After `Close` the connection is
/// gone, and every later script call is answered with `SessionClosed`.
fn worker(
    mut conn: Connection,
    read_only: bool,
    cancel: &Arc<SqliteCancel>,
    unreported: &Unreported,
    requests: std_mpsc::Receiver<Request>,
) {
    // The handler is installed once here, since installing needs the connection mutably and
    // a script's statements borrow it for the whole script.
    let armed = Armed::default();
    let (slot, stop_cancel) = (Arc::clone(&armed), Arc::clone(cancel));
    let _ = conn.progress_handler(
        1000,
        Some(move || {
            slot.lock()
                .unwrap_or_else(PoisonError::into_inner)
                .is_some_and(|(generation, deadline)| {
                    stop_cancel.hits(generation) || deadline.is_some_and(|d| Instant::now() >= d)
                })
        }),
    );
    let mut conn = Some(conn);
    for request in requests {
        match request {
            Request::Script { sql, ctl } => {
                if conn.as_ref().is_some_and(|c| {
                    serve_script(c, read_only, cancel, unreported, &armed, &sql, &ctl)
                }) {
                    conn = None;
                }
                for msg in ctl.iter() {
                    msg.reject(ExecuteError::SessionClosed);
                }
            }
            Request::Settle { done } => {
                let _ = done.send(());
            }
            Request::Close { done } => {
                conn = None;
                let _ = done.send(());
            }
            Request::Introspect(job) => job(conn.as_ref()),
        }
    }
}

/// `current` holds the prepared, not-yet-run statement; the next `Next` finalizes it,
/// which is all `Pending::skip` needs. Returns when the script drops its sender, or `true`
/// at once when a statement's `OnTruncate::Close` asks for the connection to be closed.
fn serve_script(
    conn: &Connection,
    read_only: bool,
    cancel: &Arc<SqliteCancel>,
    unreported: &Unreported,
    armed: &Armed,
    sql: &str,
    ctl: &std_mpsc::Receiver<Ctl>,
) -> bool {
    let mut batch = Batch::new(conn, sql);
    // The prepared statement, with whether it reports a row count (see `counts_rows`).
    let mut current: Option<(Statement<'_>, bool)> = None;
    // A second batch over the same text prepares ahead, never runs, to find the last
    // statement; `prepared` counts the statements `batch` has handed out.
    let mut scout = Batch::new(conn, sql);
    let (mut prepared, mut scouted, mut scout_done) = (0usize, 0usize, false);
    // Batch does not advance past a statement that fails to prepare, so that error ends it.
    let mut ended = false;
    for msg in ctl.iter() {
        match msg {
            Ctl::Next { reply } => {
                current = None;
                let item = if ended {
                    None
                } else {
                    match batch.next() {
                        Ok(None) => None,
                        Err(e) => {
                            ended = true;
                            Some(Err(prepare_error(e, sql)))
                        }
                        Ok(Some(stmt)) => {
                            let sql = stmt.expanded_sql().unwrap_or_default().trim().to_owned();
                            let classification = sqlite_classification(&sql, stmt.readonly());
                            current = Some((stmt, counts_rows(&sql)));
                            prepared += 1;
                            Some(Ok(Prepared {
                                sql,
                                classification,
                            }))
                        }
                    }
                };
                let _ = reply.send(item);
            }
            // A statement that cannot be prepared yet, say over a table an earlier statement
            // creates, counts as following; Batch stays on it, so a later ask retries.
            Ctl::IsLast { reply } => {
                let last = loop {
                    if scout_done || scouted > prepared {
                        break scout_done && scouted == prepared;
                    }
                    match scout.next() {
                        Ok(Some(_)) => scouted += 1,
                        Ok(None) => scout_done = true,
                        Err(_) => break false,
                    }
                };
                let _ = reply.send(last);
            }
            Ctl::Execute {
                reply,
                events,
                limits,
                timeout,
                generation,
                resume,
            } => match current.take() {
                Some((mut stmt, counts)) => {
                    let control = Control {
                        cancel: Arc::clone(cancel),
                        generation,
                        deadline: timeout.map(|t| Instant::now() + t),
                        read_only,
                        resume,
                        unreported: Arc::clone(unreported),
                        armed: Arc::clone(armed),
                    };
                    if run_statement(conn, &mut stmt, limits, control, reply, &events, counts) {
                        return true;
                    }
                }
                None => drop(reply.send(Err(ExecuteError::SessionClosed))),
            },
        }
    }
    false
}

/// What may stop one statement: its deadline, or a cancel of its generation.
struct Control {
    cancel: Arc<SqliteCancel>,
    generation: u64,
    deadline: Option<Instant>,
    /// Whether the session is read-only, so the ATTACH limit of 0 reads as a refusal.
    read_only: bool,
    /// Where a row cut under `OnTruncate::Park` waits for more allowance.
    resume: Option<std_mpsc::Receiver<Option<u64>>>,
    /// Where a dropped write leaves its failed commit for the session to report.
    unreported: Unreported,
    /// The slot the connection's progress handler polls for this statement's stop condition.
    armed: Armed,
}

impl Control {
    /// Only the progress handler interrupts, so an interrupt is a cancel of this generation
    /// or else the deadline.
    fn map_error(&self, e: rusqlite::Error) -> ExecuteError {
        match e.sqlite_error_code() {
            Some(ErrorCode::OperationInterrupted) if self.cancel.hits(self.generation) => {
                ExecuteError::Cancelled
            }
            Some(ErrorCode::OperationInterrupted) => ExecuteError::Timeout,
            // `open` sets the ATTACH limit to 0 on a read-only session; report that as a refusal.
            _ if self.read_only && e.to_string().contains("too many attached databases") => {
                ExecuteError::ReadOnlyViolation {
                    message: "ATTACH is refused on a read-only session".to_owned(),
                }
            }
            _ => map_error(e),
        }
    }
}

/// Runs one statement under a progress handler that stops it at its deadline or on its
/// cancel within 1000 VM steps. Returns whether the session must close under `OnTruncate::Close`.
fn run_statement(
    conn: &Connection,
    stmt: &mut Statement<'_>,
    limits: Limits,
    control: Control,
    reply: oneshot::Sender<Result<Vec<ColumnInfo>, ExecuteError>>,
    events: &mpsc::Sender<Event>,
    counts: bool,
) -> bool {
    if control.cancel.hits(control.generation) {
        let _ = reply.send(Err(ExecuteError::Cancelled));
        return false;
    }
    let arm = |value| *control.armed.lock().unwrap_or_else(PoisonError::into_inner) = value;
    arm(Some((control.generation, control.deadline)));
    let (truncated, abandoned) = stream_rows(conn, stmt, limits, &control, reply, events, counts);
    arm(None);
    (truncated || abandoned) && limits.on_truncate == OnTruncate::Close
}

/// The first step answers `Execute`; rows then go out in batches over the depth-1 channel,
/// followed by the outcome or the failure. Stepping stops at a reached row or byte bound or
/// when the stream is dropped (a failed send); returns `(truncated, abandoned)`. Under
/// `OnTruncate::Park` a row cut instead parks and continues on the stream's extend.
fn stream_rows(
    conn: &Connection,
    stmt: &mut Statement<'_>,
    mut limits: Limits,
    control: &Control,
    reply: oneshot::Sender<Result<Vec<ColumnInfo>, ExecuteError>>,
    events: &mpsc::Sender<Event>,
    counts: bool,
) -> (bool, bool) {
    let mut ncols = stmt.column_count();
    let writes = !stmt.readonly();
    let changes_before = conn.total_changes();
    let mut reply = Some(reply);
    let mut builder = BatchBuilder::new(ncols);
    let mut scratch = vec![String::new(); ncols];
    let mut numbers = Vec::with_capacity(ncols);
    let mut rows_seen = 0u64;
    let mut truncated_by = None;
    let mut abandoned = false;
    let mut failure = None;
    {
        let mut rows = stmt.raw_query();
        loop {
            let step = rows.next();
            // The first step re-prepares a statement whose table changed since it was
            // prepared, so the shape is read only after it; a first step at the end answers
            // after the loop, once the statement is free again.
            if let Some(r) = reply.take() {
                match step {
                    Err(e) => {
                        let _ = r.send(Err(control.map_error(e)));
                        return (false, false);
                    }
                    Ok(Some(row)) => {
                        let columns = column_infos(row.as_ref());
                        ncols = columns.len();
                        builder = BatchBuilder::new(ncols);
                        scratch = vec![String::new(); ncols];
                        drop(r.send(Ok(columns)));
                    }
                    Ok(None) => reply = Some(r),
                }
            }
            match step {
                Err(e) => {
                    failure = Some(control.map_error(e));
                    break;
                }
                Ok(None) => break,
                Ok(Some(row)) => {
                    // A row beyond a reached bound exists, so the result is truncated.
                    truncated_by = limits.cut(rows_seen, builder.total_bytes());
                    if truncated_by == Some(TruncatedBy::Rows)
                        && let Some(resume) = control.resume.as_ref()
                    {
                        // Hand over the rows so far and the cut, then hold this row until the
                        // stream asks for more; a dropped stream fails the send or the receive.
                        let flushed = builder.is_empty()
                            || events.blocking_send(Event::Batch(builder.finish())).is_ok();
                        let parked = flushed
                            && events
                                .blocking_send(Event::Parked(Outcome {
                                    rows: rows_seen,
                                    truncated: true,
                                    rows_affected: None,
                                    truncated_by,
                                    extra_results: 0,
                                    rolled_back: false,
                                }))
                                .is_ok();
                        match parked.then(|| resume.recv()) {
                            Some(Ok(more)) => {
                                limits.max_rows =
                                    more.and_then(|m| limits.max_rows.map(|r| r.saturating_add(m)));
                                // The held row faces the new bounds: the byte bound may stop it.
                                truncated_by = limits.cut(rows_seen, builder.total_bytes());
                            }
                            _ => {
                                abandoned = true;
                                break;
                            }
                        }
                    }
                    if truncated_by.is_some() {
                        // A reset would swallow a write's failed commit, such as a deferred
                        // key check, so the write steps to its end and reports it.
                        if writes {
                            failure = finish_write(&mut rows).err().map(|e| control.map_error(e));
                        }
                        break;
                    }
                    let closed = match push_rendered(&mut builder, row, &mut scratch, &mut numbers)
                    {
                        Ok(closed) => closed,
                        Err(e) => {
                            failure = Some(map_error(e));
                            break;
                        }
                    };
                    rows_seen += 1;
                    let full = builder.is_full().then(|| builder.finish());
                    if closed
                        .into_iter()
                        .chain(full)
                        .any(|b| events.blocking_send(Event::Batch(b)).is_err())
                    {
                        abandoned = true;
                        break;
                    }
                }
            }
        }
        // A dropped stream's reset would swallow a write's failed commit too, so the write
        // steps to its end and leaves the failure for the session's next settle or close.
        if abandoned
            && writes
            && let Err(e) = finish_write(&mut rows)
        {
            control
                .unreported
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .get_or_insert(control.map_error(e));
        }
    }
    if let Some(r) = reply.take() {
        let columns = column_infos(stmt);
        ncols = columns.len();
        drop(r.send(Ok(columns)));
    }
    if failure.is_none()
        && !abandoned
        && !builder.is_empty()
        && events
            .blocking_send(Event::Batch(builder.finish()))
            .is_err()
    {
        abandoned = true;
    }
    let truncated = truncated_by.is_some();
    // Nobody is listening once the stream is gone; returning resets the statement.
    if abandoned {
        return (truncated, true);
    }
    // total_changes also counts trigger and cascade rows and changes() goes stale
    // after DDL, so changes() is trusted only when the counter moved; DDL counts none.
    let delta = conn.total_changes() - changes_before;
    let rows_affected =
        (ncols == 0 && (counts || delta > 0)).then(|| if delta == 0 { 0 } else { conn.changes() });
    let _ = events.blocking_send(match failure {
        Some(e) => Event::Failed(e),
        None => Event::Done(Outcome {
            rows: rows_seen,
            truncated,
            rows_affected,
            truncated_by,
            extra_results: 0,
            rolled_back: false,
        }),
    });
    (truncated, false)
}

/// Each result column of `stmt` with its declared type and the table column it reads.
fn column_infos(stmt: &Statement<'_>) -> Vec<ColumnInfo> {
    stmt.columns()
        .zip(stmt.columns_with_metadata())
        .map(|(c, m)| {
            let source = match (m.database_name(), m.table_name(), m.origin_name()) {
                (Some(schema), Some(table), Some(column)) => Some(ColumnSource::Sqlite {
                    schema: schema.to_owned(),
                    table: table.to_owned(),
                    column: column.to_owned(),
                }),
                _ => None,
            };
            ColumnInfo::new(c.name(), c.decl_type().unwrap_or(""), None).with_source(source)
        })
        .collect()
}

/// Steps a write's unread rows to the end; the last step carries its commit.
fn finish_write(rows: &mut rusqlite::Rows<'_>) -> rusqlite::Result<()> {
    while rows.next()?.is_some() {}
    Ok(())
}

/// Pushes one row, each value `rendered` into its own reused `scratch` string, marking the
/// stored numbers in the reused `numbers` buffer.
fn push_rendered(
    b: &mut BatchBuilder,
    row: &Row<'_>,
    scratch: &mut [String],
    numbers: &mut Vec<bool>,
) -> rusqlite::Result<Option<ResultBatch>> {
    numbers.clear();
    let cells = scratch
        .iter_mut()
        .enumerate()
        .map(|(i, s)| {
            let value = row.get_ref(i)?;
            numbers.push(matches!(value, ValueRef::Integer(_) | ValueRef::Real(_)));
            Ok(rendered(value, s))
        })
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(b.push_typed_row(&cells, |i| numbers[i]))
}

/// `value` as a browse cell: numbers rendered into `s`, Real in Rust's shortest round-trip
/// form, and text that is not UTF-8 as bytes.
fn rendered<'a>(value: ValueRef<'a>, s: &'a mut String) -> CellRef<'a> {
    match value {
        ValueRef::Null => CellRef::Null,
        ValueRef::Integer(n) => {
            s.clear();
            let _ = write!(s, "{n}");
            CellRef::Text(s)
        }
        ValueRef::Real(f) => {
            s.clear();
            let _ = write!(s, "{f:?}");
            CellRef::Text(s)
        }
        ValueRef::Text(t) => std::str::from_utf8(t).map_or(CellRef::Bytes(t), CellRef::Text),
        ValueRef::Blob(bytes) => CellRef::Bytes(bytes),
    }
}

/// A browse cell as the value that would match it.
fn owned(cell: CellRef<'_>) -> EditValue {
    match cell {
        CellRef::Null => EditValue::Null,
        CellRef::Text(t) => EditValue::Text(t.to_owned()),
        CellRef::Bytes(b) => EditValue::Bytes(b.to_vec()),
    }
}

/// A SQLite failure carries its extended result code; anything else has an empty code.
fn map_error(e: rusqlite::Error) -> ExecuteError {
    if e.sqlite_error_code() == Some(ErrorCode::ReadOnly) {
        return ExecuteError::ReadOnlyViolation {
            message: e.to_string(),
        };
    }
    match e {
        rusqlite::Error::SqliteFailure(f, msg) => ExecuteError::database(
            f.extended_code.to_string(),
            msg.unwrap_or_else(|| f.to_string()),
        ),
        rusqlite::Error::SqlInputError { error, msg, .. } => {
            ExecuteError::database(error.extended_code.to_string(), msg)
        }
        other => ExecuteError::database(String::new(), other.to_string()),
    }
}

/// A prepare error from `Batch` over `script`: SQLite's offset counts into the remaining
/// batch text, a suffix of the script, so it is moved into the failing statement's trimmed
/// range, the text the app records for it.
fn prepare_error(e: rusqlite::Error, script: &str) -> ExecuteError {
    let at = match &e {
        rusqlite::Error::SqlInputError { sql, offset, .. }
            if *offset >= 0 && script.ends_with(sql.as_str()) =>
        {
            Some(script.len() - sql.len() + *offset as usize)
        }
        _ => None,
    };
    let mut err = map_error(e);
    if let (Some(at), ExecuteError::Database { position, .. }) = (at, &mut err) {
        let ranges = statement_ranges(Engine::Sqlite, script);
        *position = ranges.iter().rfind(|r| r.start <= at).map(|r| {
            let text = &script[r.clone()];
            let trimmed = text.trim();
            let start = r.start + (text.len() - text.trim_start().len());
            at.saturating_sub(start).min(trimmed.len())
        });
    }
    err
}

#[cfg(test)]
mod cancel_tests {
    use super::*;

    #[test]
    fn an_older_cancel_never_lowers_a_newer_mark() {
        let cancel = SqliteCancel {
            generation: AtomicU64::new(3),
            cancelled: AtomicU64::new(5),
        };
        cancel.cancel();
        assert!(cancel.hits(5));
    }
}
