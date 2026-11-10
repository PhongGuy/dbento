//! The DuckDB worker: one OS thread owns a session's lease and serves the SQLite driver's
//! request and event protocol, splitting each script itself and preparing at `Execute`.

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc as std_mpsc};
use std::time::{Duration, Instant};

use ::duckdb::arrow::array::{Array, AsArray};
use ::duckdb::arrow::datatypes::{DataType, Int64Type, Schema};
use ::duckdb::{Connection, Statement};
use tokio::sync::{mpsc, oneshot};

use super::cancel::DuckCancel;
use super::classify::{duckdb_classification, duckdb_has_returning, duckdb_sets_session};
use super::registry::Lease;
use super::render::Renderer;
use crate::classify::leading_keyword;
use crate::introspect::quote_ident;
use crate::ranges::statement_ranges;
use crate::session::{ExecuteError, Limits, OnTruncate, Outcome, Prepared, TruncatedBy, Writes};
use crate::value::{BatchBuilder, CellRef, ColumnInfo, ResultBatch};
use crate::workspace::Engine;

#[cfg(test)]
mod tests;

/// A catalog read on the session's connection; handed `None` once the lease is gone.
pub(crate) type Job = Box<dyn FnOnce(Option<&Connection>) + Send>;

pub(crate) enum Request {
    Script {
        sql: String,
        ctl: std_mpsc::Receiver<Ctl>,
    },
    /// Answered once every earlier script has let go of its statements.
    Settle { done: oneshot::Sender<()> },
    /// Answered once the lease is dropped.
    Close { done: oneshot::Sender<()> },
    /// Run after every earlier script has let go, like `Settle`.
    Introspect(Job),
}

pub(crate) enum Ctl {
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
        /// Present under `OnTruncate::Park`: the worker waits on it at a row cut.
        resume: Option<std_mpsc::Receiver<Option<u64>>>,
        /// The stop guard's generation for this statement, taken when it was sent.
        generation: u64,
        /// The session's statement timeout, counted from the worker's `begin`.
        timeout: Option<Duration>,
    },
}

pub(crate) enum Event {
    Batch(ResultBatch),
    Done(Outcome),
    /// A row cut under `OnTruncate::Park`: the worker holds the statement until resumed.
    Parked(Outcome),
    Failed(ExecuteError),
}

/// Per-session state the async half and the worker share.
pub(crate) struct Shared {
    pub(crate) read_only: bool,
    /// Read-only on a writable instance: each statement runs in a read-only transaction.
    pub(crate) wrapped: bool,
    pub(crate) settings_changed: AtomicBool,
}

impl Shared {
    pub(crate) fn new(read_only: bool, writable: bool) -> Self {
        Shared {
            read_only,
            wrapped: read_only && writable,
            settings_changed: AtomicBool::new(false),
        }
    }
}

/// Serves requests until the session drops its sender. After `Close`, or a statement that
/// closes the session, the lease is gone and every later script call is `SessionClosed`.
pub(crate) fn serve(
    lease: Lease,
    shared: &Shared,
    cancel: &DuckCancel,
    requests: std_mpsc::Receiver<Request>,
) {
    let mut lease = Some(lease);
    for request in requests {
        match request {
            Request::Script { sql, ctl } => {
                if lease
                    .as_ref()
                    .is_some_and(|l| serve_script(l.conn(), shared, cancel, &sql, &ctl))
                {
                    lease = None;
                }
                ctl.iter().for_each(|msg| msg.reject());
            }
            Request::Settle { done } => {
                let _ = done.send(());
            }
            Request::Close { done } => {
                lease = None;
                let _ = done.send(());
            }
            Request::Introspect(job) => job(lease.as_ref().map(Lease::conn)),
        }
    }
}

impl Ctl {
    /// Answers as a closed session does.
    fn reject(self) {
        match self {
            Ctl::Next { reply } => drop(reply.send(Some(Err(ExecuteError::SessionClosed)))),
            Ctl::IsLast { reply } => drop(reply.send(true)),
            Ctl::Execute { reply, .. } => drop(reply.send(Err(ExecuteError::SessionClosed))),
        }
    }
}

/// `Next` hands out the next split statement, classified on its own text, and keeps it as
/// `current`; nothing reaches DuckDB until `Execute`, so a skipped statement never runs.
/// Returns `true` at once when a statement asks for the session to close.
fn serve_script(
    conn: &Connection,
    shared: &Shared,
    cancel: &DuckCancel,
    sql: &str,
    ctl: &std_mpsc::Receiver<Ctl>,
) -> bool {
    let ranges = statement_ranges(Engine::Duckdb, sql);
    let mut handed = 0usize;
    let mut current: Option<String> = None;
    // A refused statement ends the script: later `Next` calls find nothing.
    let mut ended = false;
    for msg in ctl.iter() {
        match msg {
            Ctl::Next { reply } => {
                current = ranges
                    .get(handed)
                    .filter(|_| !ended)
                    .map(|r| sql[r.clone()].trim().to_owned());
                handed += usize::from(current.is_some());
                let item = current.clone().map(|text| {
                    let classification = duckdb_classification(&text);
                    // Each statement already runs in its own read-only transaction.
                    if shared.wrapped && classification.transaction_control {
                        return Err(ExecuteError::TransactionControl {
                            reason: "transaction control is not available in a read-only \
                                     session; each statement runs in its own read-only \
                                     transaction"
                                .to_owned(),
                        });
                    }
                    Ok(Prepared {
                        classification,
                        sql: text,
                    })
                });
                if matches!(item, Some(Err(_))) {
                    ended = true;
                    current = None;
                }
                let _ = reply.send(item);
            }
            Ctl::IsLast { reply } => drop(reply.send(ended || handed >= ranges.len())),
            Ctl::Execute {
                reply,
                events,
                limits,
                resume,
                generation,
                timeout,
            } => match current.take() {
                Some(text) => {
                    let job = StatementJob {
                        text: &text,
                        generation,
                        timeout,
                        limits,
                        resume,
                    };
                    if run_statement(conn, shared, cancel, job, reply, &events) {
                        return true;
                    }
                }
                None => drop(reply.send(Err(ExecuteError::SessionClosed))),
            },
        }
    }
    false
}

/// One statement as `Execute` asked for it.
struct StatementJob<'a> {
    text: &'a str,
    generation: u64,
    timeout: Option<Duration>,
    limits: Limits,
    resume: Option<std_mpsc::Receiver<Option<u64>>>,
}

/// How one statement's row loop ended.
#[derive(Default)]
struct Ending {
    truncated: bool,
    abandoned: bool,
}

/// Runs one statement under a panic guard. The columns reply lives outside the guarded
/// closure, so a failure before it goes out answers `Execute` and one after is an event.
/// Returns whether the session must close.
fn run_statement(
    conn: &Connection,
    shared: &Shared,
    cancel: &DuckCancel,
    job: StatementJob<'_>,
    reply: oneshot::Sender<Result<Vec<ColumnInfo>, ExecuteError>>,
    events: &mpsc::Sender<Event>,
) -> bool {
    // The statement is stoppable from here until `end`; a cancel that already reached it
    // answers before anything runs.
    let handle = conn.interrupt_handle();
    let deadline = job.timeout.map(|t| Instant::now() + t);
    if let Err(stop) = cancel.begin(
        job.generation,
        deadline,
        Arc::new(move || handle.interrupt()),
    ) {
        drop(reply.send(Err(stop.into())));
        return false;
    }
    let limits = job.limits;
    // A wrapped session's statement starts its own read-only transaction before the prepare
    // and rolls it back on every exit, the statement's stream already dropped by then.
    if shared.wrapped
        && let Err(e) = conn.execute_batch("BEGIN TRANSACTION READ ONLY")
    {
        drop(reply.send(Err(stopped_or(cancel, &e, shared.read_only))));
        cancel.end();
        // An interrupt late in the BEGIN can fail it with its transaction still open; without
        // a rollback every later wrapped BEGIN would fail. Harmless when none is open.
        let _ = conn.execute_batch("ROLLBACK");
        return false;
    }
    let mut reply = Some(reply);
    let ran = catch_unwind(AssertUnwindSafe(|| {
        execute(conn, shared, cancel, job, &mut reply, events)
    }));
    // Ended before the rollback, so no late fire can interrupt it.
    cancel.end();
    if shared.wrapped {
        let _ = conn.execute_batch("ROLLBACK");
    }
    let failure = match ran {
        Ok(Ok(end)) => {
            let cut = end.truncated || end.abandoned;
            return cut && limits.on_truncate == OnTruncate::Close;
        }
        Ok(Err(e)) => e,
        Err(panic) => ExecuteError::database("panic", panic_text(panic.as_ref())),
    };
    match reply.take() {
        Some(r) => drop(r.send(Err(failure))),
        None => drop(events.blocking_send(Event::Failed(failure))),
    }
    false
}

/// Executes `text`, answers `reply` with its columns, then streams its rows.
fn execute(
    conn: &Connection,
    shared: &Shared,
    cancel: &DuckCancel,
    job: StatementJob<'_>,
    reply: &mut Option<oneshot::Sender<Result<Vec<ColumnInfo>, ExecuteError>>>,
    events: &mpsc::Sender<Event>,
) -> Result<Ending, ExecuteError> {
    let StatementJob {
        text,
        limits,
        resume,
        ..
    } = job;
    let fail = |e: ::duckdb::Error| stopped_or(cancel, &e, shared.read_only);
    let writes = duckdb_classification(text).writes;
    let mut st = conn.prepare(text).map_err(fail)?;
    // Executing through `stream_arrow` fixes the schema and applies any write; a SELECT does
    // its work in `step`, so its runtime errors arrive there. Its start clears earlier
    // interrupts, so the guard hears of it at once, whether it failed or not.
    let started = st.stream_arrow([]).map(drop);
    cancel.executed();
    // The crate refuses a VARIANT column inside `stream_arrow`, before any column name is
    // known, so the refusal names the column by its position.
    if let Err(::duckdb::Error::FromSqlConversionFailure(idx, ::duckdb::types::Type::Variant, _)) =
        &started
    {
        return Err(ExecuteError::Unsupported {
            reason: format!(
                "column {} has type `VARIANT`, which Dbento cannot display yet; \
                 select it with `CAST(… AS VARCHAR)` instead",
                idx + 1
            ),
        });
    }
    started.map_err(fail)?;
    if duckdb_sets_session(text) {
        shared.settings_changed.store(true, Ordering::SeqCst);
    }
    // Execution start may have cleared the cancel's every fire, so a read the cancel reached
    // fails here rather than after its columns; a write already applied its changes.
    if writes == Writes::No
        && let Some(stop) = cancel.stopped()
    {
        return Err(stop.into());
    }
    let schema = st.schema();
    if let Some(counts) = status(text, &schema) {
        let mut affected = 0u64;
        while let Some(chunk) = st.step().map_err(fail)? {
            if counts && let Some(column) = chunk.column(0).as_primitive_opt::<Int64Type>() {
                affected += column.iter().flatten().map(i64::cast_unsigned).sum::<u64>();
            }
        }
        send_columns(reply, Vec::new());
        let _ = events.blocking_send(Event::Done(outcome(0, counts.then_some(affected), None)));
        return Ok(Ending::default());
    }
    let mut renderers = Vec::with_capacity(schema.fields().len());
    for (i, field) in schema.fields().iter().enumerate() {
        match Renderer::for_column(&st.column_logical_type(i), field.data_type()) {
            Ok(r) => renderers.push(r),
            Err(u) => {
                // DuckDB applied any write at execute, so the refusal says so; only a parsed
                // write is known to have changed data, a CALL or unclassified text only may have.
                let kept = match writes {
                    Writes::No => "",
                    Writes::Yes if !leading_keyword(text).eq_ignore_ascii_case("CALL") => {
                        "; the statement's changes were applied"
                    }
                    Writes::Yes | Writes::Unknown => "; the statement may have changed data",
                };
                let name = quote_ident(Engine::Duckdb, field.name());
                return Err(ExecuteError::Unsupported {
                    reason: format!(
                        "column `{name}` has type `{}`, which Dbento cannot display yet; \
                         select `CAST({name} AS VARCHAR)` instead{kept}",
                        u.type_text
                    ),
                });
            }
        }
    }
    let columns = schema
        .fields()
        .iter()
        .zip(&renderers)
        .map(|(f, r)| ColumnInfo::new(f.name(), r.type_text(), None))
        .collect();
    send_columns(reply, columns);
    Ok(stream_rows(
        &st,
        &renderers,
        limits,
        events,
        resume.as_ref(),
        shared,
        cancel,
    ))
}

/// Sends `stmt`'s rows from its executed result over the depth-1 channel, then the outcome,
/// stopping at a row or byte cut or a dropped stream. Under `Park` a row cut waits on
/// `resume` for more allowance. A write's leftover RETURNING rows are just discarded: DuckDB
/// applies the write at execute.
fn stream_rows(
    stmt: &Statement<'_>,
    renderers: &[Renderer],
    mut limits: Limits,
    events: &mpsc::Sender<Event>,
    resume: Option<&std_mpsc::Receiver<Option<u64>>>,
    shared: &Shared,
    cancel: &DuckCancel,
) -> Ending {
    let mut builder = BatchBuilder::new(renderers.len());
    let mut scratch = vec![String::new(); renderers.len()];
    let (mut seen, mut cut, mut abandoned, mut failure) = (0u64, None, false, None);
    'chunks: loop {
        let chunk = match stmt.step() {
            Ok(Some(chunk)) => chunk,
            Ok(None) => break,
            Err(e) => {
                failure = Some(stopped_or(cancel, &e, shared.read_only));
                break;
            }
        };
        for row in 0..chunk.len() {
            // A row beyond a reached bound exists, so the result is truncated.
            cut = limits.cut(seen, builder.total_bytes());
            if cut == Some(TruncatedBy::Rows)
                && let Some(resume) = resume
            {
                // Hand over the rows so far and the cut, then hold this row until the stream
                // asks for more; a dropped stream fails the send or the receive.
                let flushed = builder.is_empty()
                    || events.blocking_send(Event::Batch(builder.finish())).is_ok();
                let parked = flushed
                    && events
                        .blocking_send(Event::Parked(outcome(seen, None, cut)))
                        .is_ok();
                match parked.then(|| resume.recv()) {
                    Some(Ok(more)) => {
                        limits.max_rows =
                            more.and_then(|m| limits.max_rows.map(|r| r.saturating_add(m)));
                        cut = limits.cut(seen, builder.total_bytes());
                    }
                    _ => {
                        abandoned = true;
                        break 'chunks;
                    }
                }
            }
            if cut.is_some() {
                break 'chunks;
            }
            // A cell whose bytes break its type's layout fails the statement rather than
            // showing a made-up value.
            let cells: Result<Vec<CellRef<'_>>, usize> = renderers
                .iter()
                .zip(scratch.iter_mut())
                .enumerate()
                .map(|(i, (r, s))| r.render(chunk.column(i).as_ref(), row, s).map_err(|_| i))
                .collect();
            let cells = match cells {
                Ok(cells) => cells,
                Err(i) => {
                    failure = Some(ExecuteError::database(
                        "",
                        format!(
                            "column {} holds a malformed `{}` value",
                            i + 1,
                            renderers[i].type_text()
                        ),
                    ));
                    break 'chunks;
                }
            };
            let closed = builder.push_typed_row(&cells, |i| renderers[i].is_number());
            seen += 1;
            let full = builder.is_full().then(|| builder.finish());
            if closed
                .into_iter()
                .chain(full)
                .any(|b| events.blocking_send(Event::Batch(b)).is_err())
            {
                abandoned = true;
                break 'chunks;
            }
        }
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
    if !abandoned {
        let _ = events.blocking_send(match failure {
            Some(e) => Event::Failed(e),
            None => Event::Done(outcome(seen, None, cut)),
        });
    }
    Ending {
        truncated: cut.is_some(),
        abandoned,
    }
}

fn send_columns(
    reply: &mut Option<oneshot::Sender<Result<Vec<ColumnInfo>, ExecuteError>>>,
    columns: Vec<ColumnInfo>,
) {
    if let Some(r) = reply.take() {
        let _ = r.send(Ok(columns));
    }
}

fn outcome(rows: u64, rows_affected: Option<u64>, cut: Option<TruncatedBy>) -> Outcome {
    Outcome {
        rows,
        truncated: cut.is_some(),
        rows_affected,
        truncated_by: cut,
        extra_results: 0,
        rolled_back: false,
        consumed_capacity: None,
    }
}

/// Keywords whose statements return rows, so a `Count` or `Success` column there is data.
const ROW_KEYWORDS: &[&str] = &[
    "SELECT",
    "FROM",
    "WITH",
    "VALUES",
    "TABLE",
    "SHOW",
    "DESCRIBE",
    "SUMMARIZE",
    "PIVOT",
    "UNPIVOT",
    "EXPLAIN",
    "PRAGMA",
    "CALL",
];

/// Statements whose count of affected rows is reported.
const COUNTING: &[&str] = &["INSERT", "UPDATE", "DELETE", "MERGE", "COPY"];

/// `Some(counts)` when the result is DuckDB's status shape rather than rows: a lone
/// `Count BIGINT` or `Success BOOLEAN` from a statement that does not return rows.
fn status(text: &str, schema: &Schema) -> Option<bool> {
    if duckdb_has_returning(text) {
        return None;
    }
    let keyword = leading_keyword(text);
    let any = |words: &[&str]| words.iter().any(|w| w.eq_ignore_ascii_case(keyword));
    let [field] = &schema.fields()[..] else {
        return None;
    };
    let shaped = matches!(
        (field.name().as_str(), field.data_type()),
        ("Count", DataType::Int64) | ("Success", DataType::Boolean)
    );
    (shaped && !any(ROW_KEYWORDS)).then(|| field.name() == "Count" && any(COUNTING))
}

/// Runs a catalog read as a statement of its own `generation`: bounded by `timeout` and
/// stoppable until it returns. It never parks, so the guard's re-firing stays on until `end`.
pub(crate) fn run_read<T>(
    conn: &Connection,
    cancel: &DuckCancel,
    generation: u64,
    timeout: Option<Duration>,
    read_only: bool,
    read: impl FnOnce(&Connection) -> ::duckdb::Result<T>,
) -> Result<T, ExecuteError> {
    let handle = conn.interrupt_handle();
    let deadline = timeout.map(|t| Instant::now() + t);
    cancel.begin(generation, deadline, Arc::new(move || handle.interrupt()))?;
    let answer = match catch_unwind(AssertUnwindSafe(|| read(conn))) {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(stopped_or(cancel, &e, read_only)),
        Err(panic) => Err(ExecuteError::database("panic", panic_text(panic.as_ref()))),
    };
    cancel.end();
    answer
}

/// A stopped statement's error is its stop, whatever DuckDB's text; any other maps as usual.
fn stopped_or(cancel: &DuckCancel, e: &::duckdb::Error, read_only: bool) -> ExecuteError {
    cancel
        .stopped()
        .map_or_else(|| map_error(e, read_only), ExecuteError::from)
}

/// DuckDB's `<Class> Error: <message>` as code and message; on a read-only session its
/// read-only and lock refusals are `ReadOnlyViolation`.
pub(crate) fn map_error(e: &::duckdb::Error, read_only: bool) -> ExecuteError {
    let text = e.to_string();
    let refused = [
        "attached in read-only mode",
        "transaction is launched in read-only mode",
        "the configuration has been locked",
    ]
    .iter()
    .any(|t| text.contains(t))
        || text.starts_with("Permission Error");
    if read_only && refused {
        return ExecuteError::ReadOnlyViolation { message: text };
    }
    // A class is a few capitalised words ("Catalog", "Invalid Input"), never a sentence.
    match text.split_once(" Error: ") {
        Some((class, message))
            if !class.is_empty()
                && class.len() <= 32
                && class.chars().all(|c| c.is_ascii_alphabetic() || c == ' ') =>
        {
            ExecuteError::database(class, message)
        }
        _ => ExecuteError::database(String::new(), text),
    }
}

fn panic_text(panic: &(dyn std::any::Any + Send)) -> String {
    panic
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| panic.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "the DuckDB worker panicked".to_owned())
}
