//! The statement loop shared by `query` and `run`, reporting each event to a [`Sink`].

use std::io;
use std::time::{Duration, Instant};

use dbento_core::{
    CancelHandle, ColumnInfo, ConnectionConfig, ExecuteError, Limits, Outcome, ReadOnlyEnforcement,
    ResultBatch, Session, SessionOptions, Unguarded, Workspace, connect, resolve_key_passphrase,
    resolve_secret, statement_ranges,
};

use crate::Exit;
use crate::confirm::{confirm, unguarded_words};
use crate::csv::CsvSink;
use crate::interrupt::Interrupt;
use crate::json::{JsonSink, described};
use crate::table::TableSink;

/// Options shared by `query` and `run`.
#[derive(clap::Args)]
pub(crate) struct ExecArgs {
    /// The output format.
    #[arg(long, value_enum, default_value_t = Format::Table)]
    pub format: Format,
    /// Stop collecting a result after N rows and mark it truncated.
    #[arg(long, value_name = "N")]
    pub max_rows: Option<u64>,
    /// Run read-only even when the connection allows writes.
    #[arg(long)]
    pub read_only: bool,
    /// Stop any statement that runs longer than this, for example `30s` or `500ms`.
    #[arg(long, value_parser = humantime::parse_duration, value_name = "DUR")]
    pub timeout: Option<Duration>,
    /// Run an UPDATE or DELETE without WHERE, a DROP or a TRUNCATE without asking.
    #[arg(long)]
    pub yes: bool,
}

#[derive(Clone, Copy, clap::ValueEnum)]
pub(crate) enum Format {
    Table,
    Json,
    Csv,
}

/// Rows a table collects when `--max-rows` is absent, since it buffers a whole result to align it.
const TABLE_DEFAULT_ROWS: u64 = 1000;

/// Receives the events of one run in order; an `Err` ends the run.
pub(crate) trait Sink {
    fn begin(&mut self, connection: &str) -> io::Result<()>;
    fn started(&mut self, index: usize, sql: &str, columns: &[ColumnInfo]) -> io::Result<()>;
    fn batch(&mut self, batch: &ResultBatch) -> io::Result<()>;
    fn finished(&mut self, outcome: &Outcome, elapsed: Duration) -> io::Result<()>;
    /// `sql` is None only when the statement's text is unknown.
    fn failed(
        &mut self,
        index: usize,
        sql: Option<&str>,
        error: &ExecuteError,
        elapsed: Duration,
    ) -> io::Result<()>;
    /// The statement was not run because nobody confirmed its unguarded write.
    fn refused(&mut self, index: usize, sql: &str, kind: Unguarded) -> io::Result<()>;
    /// `closing` is a failure the session reported only on close, such as a cut-short
    /// write's failed commit; it belongs to the run, since its statement was already reported.
    fn end(&mut self, closing: Option<&ExecuteError>) -> io::Result<()>;
}

/// Runs `sql` on `conn`, statement by statement, writing the chosen format to stdout.
/// A connect failure is an `Err` (exit 3); a statement or output failure is exit 1.
pub(crate) async fn execute(
    ws: &Workspace,
    conn: &ConnectionConfig,
    sql: &str,
    args: &ExecArgs,
) -> anyhow::Result<Exit> {
    let mut options = SessionOptions::default();
    options.force_read_only = args.read_only;
    options.statement_timeout = args.timeout;
    // Ctrl-C is taken over first, so a press during a slow connect abandons it cleanly.
    let interrupt =
        Interrupt::install().map_err(|e| anyhow::Error::new(e).context("cannot handle Ctrl-C"))?;
    let store = crate::secret::store();
    let secret = resolve_secret(ws, conn, &store, &|v| std::env::var(v).ok())?;
    options.key_passphrase = resolve_key_passphrase(ws, conn, &store, &|v| std::env::var(v).ok())?;
    let supplied = secret.is_some();
    let connected = tokio::select! {
        r = connect(ws, conn, secret, options) => r,
        () = interrupt.wait() => {
            eprintln!("interrupted");
            return Ok(Exit::Failed);
        }
    };
    let session = match connected {
        Ok(session) => session,
        Err(e) => {
            // Only a missing secret gets the hint; a wrong one is the server's to report.
            let hint = if supplied {
                None
            } else {
                crate::secret::connect_hint(ws, conn, &store)
            };
            return Err(crate::secret::with_hint(e, hint));
        }
    };
    if session.read_only_enforcement() == Some(ReadOnlyEnforcement::BestEffort) {
        eprintln!(
            "note: '{}' is read-only by best effort: SQL Server refuses writes in the database \
             only with read_only_user; set read_only_user or use a reader login for production",
            conn.name()
        );
    }
    let cancel = session.cancel_handle();
    interrupt.arm(cancel.clone());
    let stop = Stop {
        interrupt: &interrupt,
        cancel: &cancel,
    };
    let mut limits = Limits::default();
    limits.max_rows = args.max_rows.or(match args.format {
        Format::Table => Some(TABLE_DEFAULT_ROWS),
        Format::Json | Format::Csv => None,
    });
    let out = io::BufWriter::new(io::stdout());
    let name = conn.name();
    let engine = conn.engine();
    let run = Run {
        connection: name,
        sql,
        limits,
        yes: args.yes,
        stop: &stop,
    };
    let exit = match args.format {
        Format::Table => {
            let mut sink =
                TableSink::new(out, io::stderr(), engine).with_default_cap(args.max_rows.is_none());
            run_and_close(session, &run, &mut sink).await
        }
        Format::Json => run_and_close(session, &run, &mut JsonSink::new(out, engine)).await,
        Format::Csv => {
            run_and_close(session, &run, &mut CsvSink::new(out, io::stderr(), engine)).await
        }
    };
    Ok(exit)
}

/// What one run executes, and on which connection.
struct Run<'a> {
    connection: &'a str,
    sql: &'a str,
    limits: Limits,
    yes: bool,
    stop: &'a Stop<'a>,
}

/// Runs the script, closes the session, then ends the sink, so a failure known only at close
/// still reaches the output before it is finished.
async fn run_and_close(mut session: Session, run: &Run<'_>, sink: &mut impl Sink) -> Exit {
    let (result, settled) = match run_script(&mut session, run, sink).await {
        Ok((exit, settled)) => (Ok(exit), settled),
        Err(e) => (Err(e), None),
    };
    let engine = session.engine();
    if let Some(s) = &settled {
        eprintln!(
            "error: after statement {}: {}",
            s.after,
            described(&s.error, engine)
        );
    }
    // Close drains a cut-short write, so its failed commit is only known here.
    let closed = session.close().await.err();
    if let Some(e) = &closed {
        eprintln!("error: after the last statement: {}", described(e, engine));
    }
    // The run has one error slot; the settle's stop is the one that ended the script.
    let closing = settled.as_ref().map(|s| &s.error).or(closed.as_ref());
    let failed = closing.is_some();
    // An output failure already broke the stream, so the sink is not ended after one.
    let ended = result.and_then(|exit| sink.end(closing).map(|()| exit));
    let exit = ended.unwrap_or_else(|e| {
        eprintln!("error: cannot write the output: {e}");
        Exit::Failed
    });
    if failed { exit.max(Exit::Failed) } else { exit }
}

/// A failure settling what a finished statement left owed, such as its drain past the cap,
/// met while preparing the next one. It belongs to the run, not to a statement.
struct Settled {
    /// The finished statement it follows, counted from 1.
    after: usize,
    error: ExecuteError,
}

/// The Ctrl-C state and the handle that stops the session's statement.
struct Stop<'a> {
    interrupt: &'a Interrupt,
    cancel: &'a CancelHandle,
}

/// Streams every statement to `sink`, stopping at the first failure or Ctrl-C; the caller
/// ends the sink with the [`Settled`] failure, if any.
async fn run_script(
    session: &mut Session,
    run: &Run<'_>,
    sink: &mut impl Sink,
) -> io::Result<(Exit, Option<Settled>)> {
    let Run {
        connection,
        sql,
        limits,
        yes,
        stop,
    } = *run;
    sink.begin(connection)?;
    // A read-only session needs no prompt: the write is refused, by the engine, or by Dbento
    // itself on a best-effort SQL Server session.
    let ask = !session.is_read_only();
    let mut exit = Exit::Ok;
    // A statement that fails to prepare has no `Pending`, so its text comes from the ranges.
    let ranges = statement_ranges(session.engine(), sql);
    let mut script = session.script(sql);
    let mut index = 0;
    let mut settled = None;
    'statements: loop {
        if stop.interrupt.stopped() {
            break;
        }
        let Some(next) = script.next().await else {
            break;
        };
        let started = Instant::now();
        let pending = match next {
            Ok(p) => p,
            Err(e) => {
                exit = Exit::Failed;
                // Past the script's end, or stopped at the drain cap (only a drain raises it),
                // the error came from settling the previous statement, not from this one.
                let text = ranges.get(index).map(|r| sql[r.clone()].trim());
                let settling = text.is_none() || matches!(e, ExecuteError::StoppedAtDrainCap);
                if settling && index > 0 {
                    settled = Some(Settled {
                        after: index,
                        error: e,
                    });
                } else {
                    sink.failed(index, text, &e, started.elapsed())?;
                }
                break;
            }
        };
        let text = pending.sql().to_owned();
        if let Some(&kind) = pending.classification().unguarded.first().filter(|_| ask) {
            let (phrase, _) = unguarded_words(kind);
            if !confirm(
                &format!("{phrase} on '{connection}'; run it?"),
                yes,
                stop.interrupt,
            )
            .await
            {
                pending.skip();
                sink.refused(index, &text, kind)?;
                exit = Exit::Refused;
                break;
            }
        }
        let mut stream = match pending.execute(limits).await {
            Ok(s) => s,
            Err(e) => {
                sink.failed(index, Some(&text), &e, started.elapsed())?;
                exit = Exit::Failed;
                break;
            }
        };
        // A press between the check above and `execute` marked the previous statement;
        // repeating the cancel reaches this one.
        if stop.interrupt.stopped() {
            stop.cancel.cancel();
        }
        sink.started(index, &text, stream.columns())?;
        while let Some(batch) = match stream.next_batch().await {
            Ok(b) => b,
            Err(e) => {
                sink.failed(index, Some(&text), &e, started.elapsed())?;
                exit = Exit::Failed;
                break 'statements;
            }
        } {
            sink.batch(&batch)?;
        }
        // The driver always ends a finished stream with an outcome; reaching this is a bug
        // in it, not a write failure.
        let Some(outcome) = stream.outcome().cloned() else {
            eprintln!("error: statement {index} finished without an outcome");
            // Reported through the sink as well, so a started JSON statement is closed.
            sink.failed(
                index,
                Some(&text),
                &ExecuteError::SessionClosed,
                started.elapsed(),
            )?;
            exit = Exit::Failed;
            break;
        };
        sink.finished(&outcome, started.elapsed())?;
        index += 1;
    }
    if stop.interrupt.stopped() {
        eprintln!("interrupted");
        exit = exit.max(Exit::Failed);
    }
    Ok((exit, settled))
}
