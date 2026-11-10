//! The connection thread: it logs in, then serves one script at a time on its tiberius client,
//! cutting each response's token stream into items. It ends when the session drops its
//! request sender.

use std::fmt::Display;
use std::future::Future;
use std::pin::{Pin, pin};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use futures_util::TryStreamExt as _;
use futures_util::future::{Either, select};
use tiberius::{
    Client, ColumnData, Config, DoneItem, DoneKind, QueryItem, QueryStream, ResultMetadata, Row,
    ToSql,
};
use tokio::net::TcpStream;
use tokio::sync::{mpsc, oneshot};
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt as _};

use super::types::{render, wire_type};
use super::{
    ATTENTION_BOUND, CONNECT_TIMEOUT, MssqlCancel, Stop, StreamError, connect_error,
    database_error, product_version, stream_error,
};
use crate::classify::mssql::{mssql_changes_settings, mssql_classification, mssql_single_query};
use crate::introspect::{quote_ident, quote_literal};
use crate::ranges::mssql_ranges;
use crate::session::{
    Classification, ConnectError, DRAIN_CAP, ExecuteError, Flavor, Limits, OnTruncate, Outcome,
    Prepared, ServerInfo, TruncatedBy, Writes,
};
use crate::value::{BatchBuilder, ColumnInfo, ResultBatch};
use crate::workspace::Engine;

type MsClient = Client<Compat<TcpStream>>;

/// TDS's CurCmd for SELECT: a counted DONE under it without metadata is a variable assignment.
const CUR_CMD_SELECT: u16 = 193;

pub(crate) enum Request {
    Script {
        sql: String,
        ctl: mpsc::UnboundedReceiver<Ctl>,
    },
    /// Answered with the drain error no caller has reported.
    Settle {
        done: oneshot::Sender<Result<(), ExecuteError>>,
    },
    Close {
        done: oneshot::Sender<Result<(), ExecuteError>>,
    },
    /// Dbento's own SQL with `params` as @P1.., on the connection's database; every selected
    /// column must be a string type, and each row comes back as text or NULL.
    Catalog {
        sql: String,
        params: Vec<String>,
        reply: oneshot::Sender<Result<Vec<Vec<Option<String>>>, ExecuteError>>,
    },
    /// `DB_NAME()` in the session's own database, when a USE moved it off the login's.
    Moved {
        reply: oneshot::Sender<Option<String>>,
    },
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
        /// Present under `OnTruncate::Park`: the thread waits on it at a row cut.
        resume: Option<mpsc::UnboundedReceiver<Option<u64>>>,
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
    /// A row cut under `OnTruncate::Park`: the thread holds the response until resumed.
    Parked(Outcome),
    Failed(ExecuteError),
}

/// The session's rules the thread enforces on every send.
pub(crate) struct Rules {
    pub(crate) timeout: Option<Duration>,
    /// Every send is refused or fenced; see `refusal` and `open_fence`.
    pub(crate) read_only: bool,
    /// The principal a read-only session impersonates, so the database refuses its writes.
    pub(crate) reader: Option<String>,
    /// The driver's flag, set before a batch with a USE or session SET is sent.
    pub(crate) settings_changed: Arc<AtomicBool>,
}

/// Starts the session's thread on a current-thread runtime of its own and waits for its
/// login; returns the session's request channel and its cancel, built on the logged-in client.
pub(crate) async fn start(
    config: Config,
    host: String,
    target: String,
    rules: Rules,
) -> Result<(mpsc::UnboundedSender<Request>, ServerInfo, Arc<MssqlCancel>), ConnectError> {
    let open = |message: String| ConnectError::Open { message };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| open(format!("cannot start the SQL Server runtime: {e}")))?;
    let (requests, rx) = mpsc::unbounded_channel();
    let (opened_tx, opened_rx) = oneshot::channel();
    std::thread::Builder::new()
        .name("dbento-mssql".into())
        .spawn(move || {
            runtime.block_on(async move {
                let sign_in = login(config, &host, &target, rules.reader.as_deref());
                let opened = tokio::time::timeout(CONNECT_TIMEOUT, sign_in)
                    .await
                    .unwrap_or_else(|_| {
                        Err(open(format!(
                            "{target}: the server did not answer within {} s",
                            CONNECT_TIMEOUT.as_secs()
                        )))
                    });
                match opened {
                    Ok((client, server, database)) => {
                        let cancel = Arc::new(MssqlCancel::new(client.attention_handle()));
                        if opened_tx.send(Ok((server, Arc::clone(&cancel)))).is_ok() {
                            let session = Thread {
                                cancel,
                                rules,
                                database,
                            };
                            serve(client, &session, rx).await;
                        }
                    }
                    Err(e) => drop(opened_tx.send(Err(e))),
                }
            });
        })
        .map_err(|e| open(format!("cannot start the SQL Server thread: {e}")))?;
    let (server, cancel) = opened_rx
        .await
        .map_err(|_| open("the SQL Server thread stopped while connecting".to_owned()))??;
    Ok((requests, server, cancel))
}

/// TCP, the TDS login, the version and database query, and the switch to `reader` if there is
/// one; any failure names the target.
async fn login(
    config: Config,
    host: &str,
    target: &str,
    reader: Option<&str>,
) -> Result<(MsClient, ServerInfo, String), ConnectError> {
    let failed = |e: &dyn Display| ConnectError::Open {
        message: format!("{target}: {e}"),
    };
    let refused = |e: tiberius::error::Error| connect_error(host, target, &e);
    let tcp = TcpStream::connect(config.get_addr())
        .await
        .map_err(|e| failed(&e))?;
    tcp.set_nodelay(true).map_err(|e| failed(&e))?;
    let mut client = Client::connect(config, tcp.compat_write())
        .await
        .map_err(refused)?;
    let row = client
        .simple_query("SELECT CAST(SERVERPROPERTY('ProductVersion') AS nvarchar(128)), DB_NAME()")
        .await
        .map_err(refused)?
        .into_row()
        .await
        .map_err(refused)?;
    let text = row.as_ref().and_then(|r| r.get::<&str, _>(0));
    let version = text
        .and_then(product_version)
        .ok_or_else(|| failed(&format!("unexpected ProductVersion {text:?}")))?;
    let database = row
        .as_ref()
        .and_then(|r| r.get::<&str, _>(1))
        .ok_or_else(|| failed(&"DB_NAME() answered NULL"))?
        .to_owned();
    let server = ServerInfo {
        flavor: Flavor::SqlServer,
        version,
    };
    if let Some(name) = reader {
        become_reader(&mut client, target, name).await?;
    }
    Ok((client, server, database))
}

/// What the impersonated principal holds that lets it write: db_owner, then the database-wide
/// rights. The ALTER ANY and CREATE families are what db_ddladmin, db_securityadmin and
/// db_accessadmin hold; none of them has ALTER itself. Table-level grants are the DBA's.
const WRITE_RIGHTS: &str = "SELECT 0 AS r, N'db_owner' AS held WHERE IS_MEMBER('db_owner') = 1 \
     UNION ALL SELECT 1, permission_name FROM sys.fn_my_permissions(NULL, 'DATABASE') \
     WHERE permission_name IN ('INSERT', 'UPDATE', 'DELETE', 'ALTER', 'CONTROL') \
     OR permission_name LIKE 'ALTER ANY %' OR permission_name LIKE 'CREATE %' \
     ORDER BY r, held";

/// Impersonates `name` for the rest of the session and confirms it took and cannot write, so
/// a session that reports the database's enforcement really has it. `NO REVERT` is only allowed
/// ad hoc, hence the plain batches.
async fn become_reader(
    client: &mut MsClient,
    target: &str,
    name: &str,
) -> Result<(), ConnectError> {
    use tiberius::error::Error;
    let failed = |why: &dyn Display| ConnectError::Open {
        message: format!("{target}: read_only_user {name:?}: {why}"),
    };
    let refused = |e: Error| match &e {
        Error::Server(t) => failed(&format_args!("{} ({})", t.message(), t.code())),
        other => failed(other),
    };
    let switch = format!(
        "EXECUTE AS USER = {} WITH NO REVERT",
        quote_literal(Engine::Mssql, name)
    );
    client
        .simple_query(switch)
        .await
        .map_err(refused)?
        .into_results()
        .await
        .map_err(refused)?;
    let row = client
        .simple_query("SELECT USER_NAME()")
        .await
        .map_err(refused)?
        .into_row()
        .await
        .map_err(refused)?;
    let user = row.as_ref().and_then(|r| r.get::<&str, _>(0));
    if user != Some(name) {
        return Err(failed(&format_args!("the session runs as {user:?}")));
    }
    let rows = client
        .simple_query(WRITE_RIGHTS)
        .await
        .map_err(refused)?
        .into_first_result()
        .await
        .map_err(refused)?;
    if rows.is_empty() {
        return Ok(());
    }
    let held: Vec<&str> = rows.iter().filter_map(|r| r.get(1)).take(3).collect();
    Err(failed(&format_args!(
        "this principal can write, so it cannot enforce read-only; it holds {} ({} in all)",
        held.join(", "),
        rows.len()
    )))
}

/// How long a catalog call may run before Dbento stops it with `Timeout`.
const CATALOG_BOUND: Duration = Duration::from_secs(30);

/// Runs a catalog statement without touching the user's transaction: a lock wait ends in 1222,
/// a statement error, and SET options made in dynamic SQL revert when it returns.
const CATALOG_SETTINGS: &str =
    "SET LOCK_TIMEOUT 5000; SET XACT_ABORT OFF; SET IMPLICIT_TRANSACTIONS OFF; ";

/// What the thread holds for the whole session: its cancel, its rules, and the login's
/// database, which every catalog call reads.
struct Thread {
    cancel: Arc<MssqlCancel>,
    rules: Rules,
    database: String,
}

/// Serves requests until the session drops its sender. Once the client is gone, by `Close`
/// or a lost connection, every later script or catalog call is answered with `SessionClosed`.
async fn serve(client: MsClient, session: &Thread, mut requests: mpsc::UnboundedReceiver<Request>) {
    let mut client = Some(client);
    let mut owed = Owed::default();
    while let Some(request) = requests.recv().await {
        match request {
            Request::Script { sql, mut ctl } => {
                let served = match client.as_mut() {
                    Some(c) => serve_script(c, session, &sql, &mut ctl, &mut owed).await,
                    None => Ok(()),
                };
                session.cancel.end();
                if let Err(Lost(e)) = served {
                    owed.keep(e);
                    client = None;
                }
                // The first `Next` after the client is gone reports what no caller has read.
                while let Some(msg) = ctl.recv().await {
                    let e = match msg {
                        Ctl::Next { .. } => owed.take(),
                        _ => None,
                    };
                    msg.reject(e.unwrap_or(ExecuteError::SessionClosed));
                }
            }
            Request::Settle { done } => {
                let answer = match (owed.take(), &client) {
                    (Some(e), _) => Err(e),
                    (None, Some(_)) => Ok(()),
                    (None, None) => Err(ExecuteError::SessionClosed),
                };
                let _ = done.send(answer);
            }
            Request::Close { done } => {
                client = None;
                let _ = done.send(owed.take().map_or(Ok(()), Err));
            }
            // A catalog call leaves what is owed for the settle that owns it.
            Request::Catalog { sql, params, reply } => {
                let answer = match client.as_mut() {
                    Some(c) => catalog(c, session, &sql, &params).await,
                    None => Ok(Err(ExecuteError::SessionClosed)),
                };
                // An RPC response ends in DONEPROC, so only this ends the call's send.
                session.cancel.end();
                let answer = answer.unwrap_or_else(|Lost(e)| {
                    client = None;
                    Err(e)
                });
                let _ = reply.send(answer);
            }
            Request::Moved { reply } => {
                let answer = match client.as_mut() {
                    Some(c) => catalog_in(c, session, None, "SELECT DB_NAME()", &[]).await,
                    None => Ok(Err(ExecuteError::SessionClosed)),
                };
                session.cancel.end();
                let now = match answer {
                    Ok(Ok(rows)) => rows.into_iter().next().and_then(|r| r.into_iter().next()),
                    Ok(Err(_)) => None,
                    Err(_) => {
                        client = None;
                        None
                    }
                };
                let _ = reply.send(now.flatten().filter(|db| *db != session.database));
            }
        }
    }
}

/// Runs `sql` on the login's database through that database's `sys.sp_executesql`, which
/// leaves the session's own database as it was; a `USE` inside the RPC would stick. The call is
/// a send of its own under `CATALOG_BOUND`, and the statement, behind `CATALOG_SETTINGS`,
/// travels as the last parameter after `params`. Returns the first result's cells as text, or
/// the stop's cause; a tiberius error that leaves the client unusable is `Lost`.
async fn catalog(
    client: &mut MsClient,
    session: &Thread,
    sql: &str,
    params: &[String],
) -> Result<Result<Vec<Vec<Option<String>>>, ExecuteError>, Lost> {
    catalog_in(client, session, Some(&session.database), sql, params).await
}

/// `catalog` through `database`'s `sp_executesql`, or for None the unqualified one, which runs
/// in whatever database the session's USE left it in.
async fn catalog_in(
    client: &mut MsClient,
    session: &Thread,
    database: Option<&str>,
    sql: &str,
    params: &[String],
) -> Result<Result<Vec<Vec<Option<String>>>, ExecuteError>, Lost> {
    let n = params.len();
    let prefix = database.map_or(String::new(), |d| {
        format!("{}.", quote_ident(Engine::Mssql, d))
    });
    let mut text = format!("EXEC {prefix}sys.sp_executesql @P{}", n + 1);
    if n > 0 {
        let decl: Vec<String> = (1..=n).map(|i| format!("@P{i} nvarchar(max)")).collect();
        let args: Vec<String> = (1..=n).map(|i| format!("@P{i}")).collect();
        text.push_str(&format!(", N'{}', {}", decl.join(", "), args.join(", ")));
    }
    let statement = format!("{CATALOG_SETTINGS}{sql}");
    let bound: Vec<&dyn ToSql> = params
        .iter()
        .map(|p| p as &dyn ToSql)
        .chain([&statement as &dyn ToSql])
        .collect();
    let cancel = &session.cancel;
    let guard = Guard {
        cancel,
        g: cancel.begin(),
    };
    cancel.arm(guard.g, CATALOG_BOUND);
    let stop = pin!(cancel.until_stopped(guard.g));
    let mut read = Box::pin(async {
        match client.query(text, &bound).await {
            Ok(stream) => stream.into_first_result().await,
            Err(e) => Err(e),
        }
    });
    // The call writes and reads in one future, which must not be cut mid-write; a stop goes on
    // reading through the acknowledgement, repeated in case the write forgot it.
    let rows = match select(read.as_mut(), stop).await {
        Either::Left((rows, _)) => rows,
        Either::Right(_) => {
            cancel.sent(guard.g);
            tokio::time::timeout_at(guard.deadline(), read.as_mut())
                .await
                .unwrap_or(Ok(Vec::new()))
        }
    };
    drop(read);
    if let Some(why) = guard.stopped() {
        drain_stopped(client, guard)
            .await
            .map_err(|_| Lost(why.error()))?;
        return Ok(Err(why.error()));
    }
    let rows = match rows {
        Ok(rows) => rows,
        Err(e) => return read_error(e, session.rules.reader.is_some()).map(Err),
    };
    let texts = rows
        .iter()
        .map(|row| {
            (0..row.len())
                .map(|i| {
                    row.try_get::<&str, usize>(i)
                        .map(|cell| cell.map(str::to_owned))
                        .map_err(|_| ExecuteError::Unsupported {
                            reason: format!("catalog column {i} is not a string type"),
                        })
                })
                .collect()
        })
        .collect();
    Ok(texts)
}

/// The connection broke mid-response, tiberius lost its place in it, or a drain passed
/// `DRAIN_CAP`, so the client must be dropped; the error is what nobody has been told yet,
/// or `SessionClosed` once it was delivered.
struct Lost(ExecuteError);

/// Errors no caller has read, oldest first: at most one statement error, then at most one
/// session-closing error. `SessionClosed` is kept only by `keep_closed`, when no caller could
/// see the loss. Each `Next`, settle or close takes one.
/// A fence verdict takes the statement slot from an older error, since writes may have committed.
#[derive(Default)]
struct Owed {
    statement: Option<ExecuteError>,
    closing: Option<ExecuteError>,
    /// The send in progress met an error, delivered or kept, so its fence count is not judged.
    send_failed: bool,
}

impl Owed {
    /// Keeps `e` unless its slot is taken; `SessionClosed` says nothing the closed session
    /// will not say itself.
    fn keep(&mut self, e: ExecuteError) {
        self.send_failed = true;
        match e {
            ExecuteError::SessionClosed => {}
            e @ ExecuteError::ReadOnlyViolation { .. } => self.statement = Some(e),
            e if e.closes_session() => drop(self.closing.get_or_insert(e)),
            e => drop(self.statement.get_or_insert(e)),
        }
    }

    /// Records a client lost with no caller left to meet it, so a close cannot read as clean.
    fn keep_closed(&mut self) {
        self.send_failed = true;
        self.closing.get_or_insert(ExecuteError::SessionClosed);
    }

    fn take(&mut self) -> Option<ExecuteError> {
        self.statement.take().or_else(|| self.closing.take())
    }
}

/// A drain's result, for the error in it no caller has read.
trait Found {
    fn found(self) -> Option<ExecuteError>;
}

impl Found for Option<ExecuteError> {
    fn found(self) -> Option<ExecuteError> {
        self
    }
}

impl Found for (u32, Option<ExecuteError>) {
    fn found(self) -> Option<ExecuteError> {
        self.1
    }
}

/// Bounds a drain of `guard`'s send by `DRAIN_CAP`. Past it the send gets an Attention and
/// the same drain reads on through the acknowledgement within `ATTENTION_BOUND`; an error it
/// found is owed, and the client is lost with `StoppedAtDrainCap`.
async fn capped<T: Found>(
    guard: Guard<'_>,
    owed: &mut Owed,
    drain: impl Future<Output = Result<T, Lost>>,
) -> Result<T, Lost> {
    let mut drain = pin!(drain);
    if let Ok(drained) = tokio::time::timeout(DRAIN_CAP, drain.as_mut()).await {
        return drained;
    }
    guard.cancel.attend(guard.g);
    if let Ok(Ok(drained)) = tokio::time::timeout(ATTENTION_BOUND, drain).await
        && let Some(e) = drained.found()
    {
        owed.keep(e);
    }
    Err(Lost(ExecuteError::StoppedAtDrainCap))
}

/// A tiberius error mid-response: a server error belongs to the statement, anything else
/// loses the client.
fn read_error(e: tiberius::error::Error, reader: bool) -> Result<ExecuteError, Lost> {
    let StreamError { error, closes } = stream_error(&e, reader);
    if closes { Err(Lost(error)) } else { Ok(error) }
}

/// One send of the script: a batch's trimmed text, its classification, and whether its first
/// result is the script's last (a lone query in the last batch, sent once).
struct Send<'s> {
    text: &'s str,
    class: Classification,
    last: bool,
}

/// The script's batches in order, each repeated as its GO count asks.
fn sends(sql: &str) -> Vec<Send<'_>> {
    let batches = mssql_ranges(sql);
    let count = batches.len();
    let mut out = Vec::new();
    for (i, batch) in batches.into_iter().enumerate() {
        let text = sql[batch.range].trim();
        let class = mssql_classification(text);
        let last = i + 1 == count && batch.repeat == 1 && mssql_single_query(text);
        out.extend((0..batch.repeat).map(|_| Send {
            text,
            class: class.clone(),
            last,
        }));
    }
    out
}

/// The token that opens an item.
enum Lead {
    Set(ResultMetadata),
    Count(u64),
    Error(ExecuteError),
}

/// One token of a response, a server error already in Dbento's terms.
enum Tok {
    Meta(ResultMetadata),
    Row(Row),
    Done(DoneItem),
    Error(ExecuteError),
}

/// A response being read, with room to put back the token that opened the next item. Once
/// its send is stopped, every read must end within `ATTENTION_BOUND` of the stop; once it is
/// read to its end, the send is finished and no stop reaches it.
struct Reader<'c> {
    stream: QueryStream<'c>,
    held: Option<Tok>,
    guard: Guard<'c>,
    stop: Pin<Box<dyn Future<Output = Stop> + 'c>>,
    bound: Option<tokio::time::Instant>,
    reader: bool,
}

impl<'c> Reader<'c> {
    fn new(stream: QueryStream<'c>, guard: Guard<'c>, reader: bool) -> Self {
        Reader {
            stream,
            held: None,
            guard,
            stop: Box::pin(guard.cancel.until_stopped(guard.g)),
            bound: None,
            reader,
        }
    }

    async fn next(&mut self) -> Result<Option<Tok>, Lost> {
        if let Some(t) = self.held.take() {
            return Ok(Some(t));
        }
        let read = loop {
            if let Some(at) = self.bound {
                match tokio::time::timeout_at(at, self.stream.try_next()).await {
                    Ok(read) => break read,
                    Err(_) => return Err(Lost(ExecuteError::SessionClosed)),
                }
            }
            match select(self.stream.try_next(), self.stop.as_mut()).await {
                Either::Left((read, _)) => break read,
                Either::Right(_) => self.bound = Some(self.guard.deadline()),
            }
        };
        Ok(match read {
            Ok(None) => {
                self.guard.cancel.finish(self.guard.g);
                None
            }
            Ok(Some(QueryItem::Metadata(m))) => Some(Tok::Meta(m)),
            Ok(Some(QueryItem::Row(r))) => Some(Tok::Row(r)),
            // The response's final DONE ends the send. A stop already recorded still answers,
            // since finishing only stops new ones from landing.
            Ok(Some(QueryItem::Done(d))) => {
                if d.kind == DoneKind::Done && !d.more {
                    self.guard.cancel.finish(self.guard.g);
                }
                Some(Tok::Done(d))
            }
            Ok(Some(QueryItem::Error(e))) => Some(Tok::Error(database_error(&e, self.reader))),
            Err(e) => Some(Tok::Error(read_error(e, self.reader)?)),
        })
    }

    /// Reads to the next item's opening token. A DONEPROC, an uncounted DONE, a counted
    /// SELECT without metadata (a variable assignment) and the Attention acknowledgement open
    /// nothing.
    async fn lead(&mut self) -> Result<Option<Lead>, Lost> {
        while let Some(t) = self.next().await? {
            match t {
                Tok::Meta(m) => return Ok(Some(Lead::Set(m))),
                Tok::Error(e) => return Ok(Some(Lead::Error(e))),
                Tok::Done(d)
                    if d.kind != DoneKind::DoneProc
                        && d.cur_cmd != CUR_CMD_SELECT
                        && !d.attention =>
                {
                    if let Some(n) = d.rows {
                        return Ok(Some(Lead::Count(n)));
                    }
                }
                Tok::Row(_) | Tok::Done(_) => {}
            }
        }
        Ok(None)
    }

    /// Reads past the rest of an open result set; returns a server error found in it.
    async fn skip_set(&mut self) -> Result<Option<ExecuteError>, Lost> {
        while let Some(t) = self.next().await? {
            match t {
                Tok::Row(_) => {}
                Tok::Done(d) if d.kind == DoneKind::DoneProc => {}
                Tok::Done(_) => break,
                Tok::Meta(m) => {
                    self.held = Some(Tok::Meta(m));
                    break;
                }
                Tok::Error(e) => return Ok(Some(e)),
            }
        }
        Ok(None)
    }

    /// Reads the rest of the response; returns the result sets in it and its first server error.
    async fn drain(&mut self) -> Result<(u32, Option<ExecuteError>), Lost> {
        let (mut sets, mut error) = (0u32, None);
        while let Some(t) = self.next().await? {
            match t {
                Tok::Meta(_) => sets += 1,
                Tok::Error(e) => {
                    error.get_or_insert(e);
                }
                Tok::Row(_) | Tok::Done(_) => {}
            }
        }
        Ok((sets, error))
    }
}

/// The reply a `Next` waits on.
type NextReply = oneshot::Sender<Option<Result<Prepared, ExecuteError>>>;

/// Serves one script until it drops its sender. `Next` hands out each send's first result
/// under a generation of its own, and executing it sends the batch; skipping it skips that
/// send. An error, or a stop aimed at the send handed out, ends the script.
async fn serve_script(
    client: &mut MsClient,
    session: &Thread,
    sql: &str,
    ctl: &mut mpsc::UnboundedReceiver<Ctl>,
    owed: &mut Owed,
) -> Result<(), Lost> {
    let cancel = &session.cancel;
    let sends = sends(sql);
    let mut queue = sends.iter();
    let mut handed: Option<&Send<'_>> = None;
    let mut g = 0;
    // A `Next` that reached the end of a response is answered here with the next send.
    let mut carried: Option<NextReply> = None;
    loop {
        let msg = match carried.take() {
            Some(reply) => Ctl::Next { reply },
            None => match ctl.recv().await {
                Some(msg) => msg,
                None => return Ok(()),
            },
        };
        match msg {
            Ctl::Next { reply } => {
                if handed.is_some()
                    && let Some(why) = cancel.stopped(g)
                {
                    let _ = reply.send(Some(Err(why.error())));
                    (handed, queue) = (None, Default::default());
                    cancel.end();
                    continue;
                }
                handed = queue.next();
                match handed {
                    Some(_) => g = cancel.begin(),
                    None => cancel.end(),
                }
                let _ = reply.send(handed.map(|send| {
                    Ok(Prepared {
                        sql: send.text.to_owned(),
                        classification: send.class.clone(),
                    })
                }));
            }
            Ctl::IsLast { reply } => drop(reply.send(handed.is_none_or(|send| send.last))),
            Ctl::Execute {
                reply,
                events,
                limits,
                resume,
            } => {
                let Some(send) = handed.take() else {
                    let _ = reply.send(Err(ExecuteError::SessionClosed));
                    continue;
                };
                if session.rules.read_only
                    && let Some(refused) = refusal(&send.class, session.rules.reader.is_some())
                {
                    let _ = reply.send(Err(refused));
                    queue = Default::default();
                    continue;
                }
                // Marked before sending: a USE or SET in a batch that then fails may still have taken effect.
                if mssql_changes_settings(send.text) {
                    session.rules.settings_changed.store(true, Ordering::SeqCst);
                }
                let guard = Guard { cancel, g };
                let mut out = Out::new(reply, &events, limits, resume, guard);
                // A stop before the send leaves it unsent, so there is nothing to fence.
                let fenced = session.rules.read_only && guard.stopped().is_none();
                let fence = if fenced {
                    match open_fence(client, guard).await {
                        Ok(()) => Some(client.transaction_descriptor()),
                        Err(lost) => {
                            out.fail(lost.0, owed).await;
                            return Err(Lost(ExecuteError::SessionClosed));
                        }
                    }
                } else {
                    None
                };
                owed.send_failed = false;
                let after = serve_send(client, session, guard, send, out, ctl, owed).await;
                cancel.end();
                let mut after = after?;
                if let Some(opened) = fence {
                    after = close_fence(client, guard, opened, after, owed).await?;
                }
                match after {
                    After::Cut { .. } => return Err(Lost(ExecuteError::SessionClosed)),
                    After::Next(reply) => carried = Some(reply),
                    After::Ended => queue = Default::default(),
                    After::Gone => return Ok(()),
                }
            }
        }
    }
}

/// Why a read-only session will not send a batch: always for transaction control, and for a
/// write unless the database itself would refuse it. `Unknown` is never refused on its own.
fn refusal(class: &Classification, reader: bool) -> Option<ExecuteError> {
    if class.transaction_control {
        return Some(ExecuteError::TransactionControl {
            reason: "transaction control is not available in a read-only SQL Server session; \
                     Dbento fences each batch in its own transaction"
                .to_owned(),
        });
    }
    (class.writes == Writes::Yes && !reader).then(|| ExecuteError::ReadOnlyViolation {
        message: "SQL Server cannot refuse this write without read_only_user, so Dbento refused \
                  the batch unsent; set read_only_user to let the database refuse writes"
            .to_owned(),
    })
}

/// Leaves `@@TRANCOUNT` at `FENCE_BASELINE` around a read-only send.
const FENCE_OPEN: &str = "SET IMPLICIT_TRANSACTIONS ON; SET XACT_ABORT ON; BEGIN TRANSACTION";

/// Undoes the session switches that would swallow it, reports the count, then rolls back.
const FENCE_CLOSE: &str = "SET NOEXEC OFF; SET FMTONLY OFF; SET PARSEONLY OFF; \
                           SELECT @@TRANCOUNT; IF @@TRANCOUNT > 0 ROLLBACK";

const FENCE_BASELINE: i32 = 2;

const LOW_COUNT: &str = "the batch ended Dbento's transaction, so writes may have committed";

const UNCONFIRMED_CUT: &str = "Dbento could not confirm how the batch ended after cutting its \
                               result, so the session was closed; writes may have committed";

const BAD_CLOSE_OUT: &str = "Dbento's close-out of the batch did not answer with its transaction \
                             count, so the session was closed, which rolls back what is open";

/// Runs the fence's setup ahead of `guard`'s send. Whatever keeps it from being confirmed loses
/// the client, because the user's text must never run outside it. A stop lands here too, so
/// its acknowledgement is read before the send is abandoned.
async fn open_fence(client: &mut MsClient, guard: Guard<'_>) -> Result<(), Lost> {
    let open = async {
        let mut stream = client.simple_query_items(FENCE_OPEN).await?;
        guard.cancel.sent(guard.g);
        let mut refused = false;
        while let Some(item) = stream.try_next().await? {
            refused |= matches!(item, QueryItem::Error(_));
        }
        Ok::<_, tiberius::error::Error>(refused)
    };
    let opened = tokio::time::timeout(DRAIN_CAP, open).await;
    if guard.stopped().is_some() {
        return drain_stopped(client, guard).await.map(|_| ());
    }
    match opened {
        Ok(Ok(false)) => Ok(()),
        _ => Err(Lost(ExecuteError::ReadOnlyViolation {
            message: "Dbento could not open its transaction around the batch, so the batch was \
                      not sent and the session was closed"
                .to_owned(),
        })),
    }
}

/// Sends the close-out and returns its count; `None` for any other answer, for none in
/// `DRAIN_CAP`, and for a broken connection, so no failure here reads as a clean batch.
async fn close_out(client: &mut MsClient) -> Option<i32> {
    let read = async {
        let mut stream = client.simple_query_items(FENCE_CLOSE).await?;
        let (mut sets, mut columns, mut cells) = (0, 0, Vec::new());
        while let Some(item) = stream.try_next().await? {
            match item {
                QueryItem::Metadata(m) => {
                    sets += 1;
                    columns = m.columns().len();
                }
                QueryItem::Row(row) => cells.push(match row.cells().next() {
                    Some((_, ColumnData::I32(n))) => *n,
                    _ => None,
                }),
                QueryItem::Done(_) => {}
                QueryItem::Error(_) => return Ok(None),
            }
        }
        Ok::<_, tiberius::error::Error>(close_out_count(sets, columns, &cells))
    };
    tokio::time::timeout(DRAIN_CAP, read)
        .await
        .ok()
        .and_then(Result::ok)
        .flatten()
}

/// The count in a close-out's answer, which must be one result set of one int column and one
/// row; each of `cells` is a row's first cell when it is a non-NULL int.
fn close_out_count(sets: u32, columns: usize, cells: &[Option<i32>]) -> Option<i32> {
    match (sets, columns, cells) {
        (1, 1, [Some(n)]) => Some(*n),
        _ => None,
    }
}

/// Runs the close-out after `guard`'s send and returns how the script goes on. A clean batch is
/// judged by the transaction it leaves: `opened` replaced, or a low count. After an error or a
/// stop neither proves anything, since those normally roll back, so a commit hidden in such a
/// batch goes unreported. After a cut under `OnTruncate::Close` the descriptor seen at the cut
/// stands in, and the count counts only if no Attention rolled back. A verdict answers the
/// waiting `Next` and ends the script, or is owed; a close-out of any other shape, or a cut
/// whose drain failed, loses the client, and with it the open transaction.
async fn close_fence(
    client: &mut MsClient,
    guard: Guard<'_>,
    opened: [u8; 8],
    after: After,
    owed: &mut Owed,
) -> Result<After, Lost> {
    let (seen, count_stands) = match after {
        After::Cut {
            drained: Some(attended),
            seen,
        } => (seen, !attended),
        After::Cut { drained: None, .. } => {
            return Err(Lost(ExecuteError::ReadOnlyViolation {
                message: UNCONFIRMED_CUT.to_owned(),
            }));
        }
        _ => (client.transaction_descriptor(), true),
    };
    let clean = !owed.send_failed && guard.stopped().is_none() && !matches!(after, After::Ended);
    let count = close_out(client).await;
    let low = count.is_some_and(|n| count_stands && n < FENCE_BASELINE);
    let message = match count {
        None => BAD_CLOSE_OUT,
        Some(_) if clean && (low || seen != opened) => LOW_COUNT,
        Some(_) => return Ok(after),
    };
    let verdict = ExecuteError::ReadOnlyViolation {
        message: message.to_owned(),
    };
    let gone = matches!(after, After::Gone);
    let ends = count.is_none() || matches!(after, After::Cut { .. });
    let undelivered = match after {
        After::Next(reply) => match reply.send(Some(Err(verdict))) {
            Err(Some(Err(e))) => Some(e),
            _ => None,
        },
        After::Ended | After::Gone | After::Cut { .. } => Some(verdict),
    };
    if ends {
        return Err(Lost(undelivered.unwrap_or(ExecuteError::SessionClosed)));
    }
    if let Some(e) = undelivered {
        owed.keep(e);
    }
    Ok(if gone { After::Gone } else { After::Ended })
}

/// How a send's response left the script.
enum After {
    /// The response is read out and a `Next` waits for the next send.
    Next(NextReply),
    /// An error item ended the script.
    Ended,
    /// The script dropped its sender.
    Gone,
    /// A cut under `OnTruncate::Close` stopped the send, and the session ends here. `seen` is
    /// the transaction descriptor at the cut; `drained` is the drain's answer, `None` if it
    /// failed.
    Cut {
        seen: [u8; 8],
        drained: Option<bool>,
    },
}

/// Sends one batch and serves its response: the first item on this `Execute`, then each later
/// item as a result of its own, read ahead by `Next`. A skipped later item is read past. A
/// stop before the send leaves the batch unsent; one after it reads through the Attention
/// acknowledgement, and the item being read or the next `Next` answers its cause.
async fn serve_send(
    client: &mut MsClient,
    session: &Thread,
    guard: Guard<'_>,
    send: &Send<'_>,
    mut out: Out<'_>,
    ctl: &mut mpsc::UnboundedReceiver<Ctl>,
    owed: &mut Owed,
) -> Result<After, Lost> {
    if let Some(why) = guard.stopped() {
        out.stop = Some(why);
        out.halt().await;
        return Ok(After::Ended);
    }
    if let Some(after) = session.rules.timeout {
        session.cancel.arm(guard.g, after);
    }
    let reader = session.rules.reader.is_some();
    let stream = match client.simple_query_items(send.text).await {
        Ok(stream) => {
            guard.cancel.sent(guard.g);
            stream
        }
        Err(e) => {
            return match read_error(e, reader) {
                Ok(err) => {
                    out.fail(err, owed).await;
                    Ok(After::Ended)
                }
                Err(lost) => {
                    out.fail(lost.0, owed).await;
                    Err(Lost(ExecuteError::SessionClosed))
                }
            };
        }
    };
    let mut reader = Reader::new(stream, guard, reader);
    let ran = match reader.lead().await {
        Ok(lead) => run_item(&mut reader, lead, send.last, &mut out, owed).await,
        Err(lost) => Err(lost),
    };
    match settle_item(ran, &mut reader, &mut out, owed).await? {
        Settled::Goes => {}
        Settled::Ended => return Ok(After::Ended),
        Settled::Cut => {
            drop(reader);
            return stopped(client, &mut out, ctl, owed).await;
        }
        Settled::Close => {
            drop(reader);
            return close_at_cut(client, &mut out, owed).await;
        }
    }
    let mut handed: Option<Lead> = None;
    loop {
        // The caller may sit between items with the batch still running on the server.
        let msg = match unless_stopped(guard, ctl.recv()).await {
            Ok(Some(msg)) => msg,
            Ok(None) => break,
            Err(why) => {
                drop(reader);
                return match drain_stopped(client, guard).await {
                    Ok(_) => halted(why, ctl, owed).await,
                    Err(lost) => {
                        owed.keep(why.error());
                        Err(lost)
                    }
                };
            }
        };
        match msg {
            Ctl::Next { reply } => match read_ahead(&mut reader, handed.take(), owed).await {
                // A stop answers the `Next` that finds no item after it; items the server
                // finished before the stop are still handed out.
                Ok(Ahead::Done | Ahead::Ended) if let Some(why) = guard.stopped() => {
                    drop(reader);
                    let drained = drain_stopped(client, guard).await;
                    let _ = reply.send(Some(Err(why.error())));
                    return drained.map(|_| After::Ended);
                }
                Ok(Ahead::Item(lead)) => {
                    handed = Some(lead);
                    let _ = reply.send(Some(Ok(Prepared {
                        sql: send.text.to_owned(),
                        classification: Classification {
                            writes: send.class.writes,
                            unguarded: Vec::new(),
                            transaction_control: false,
                            categories: Vec::new(),
                        },
                    })));
                }
                Ok(Ahead::Ended) => {
                    let _ = reply.send(None);
                    return Ok(After::Ended);
                }
                Ok(Ahead::Done) => return Ok(After::Next(reply)),
                Err(lost) => {
                    return Err(match reply.send(Some(Err(lost.0))) {
                        Err(Some(Err(e))) => Lost(e),
                        _ => Lost(ExecuteError::SessionClosed),
                    });
                }
            },
            Ctl::IsLast { reply } => drop(reply.send(handed.is_none())),
            Ctl::Execute {
                reply,
                events,
                limits,
                resume,
            } => {
                let mut out = Out::new(reply, &events, limits, resume, guard);
                let Some(lead) = handed.take() else {
                    out.fail(ExecuteError::SessionClosed, owed).await;
                    continue;
                };
                let ran = run_item(&mut reader, Some(lead), false, &mut out, owed).await;
                match settle_item(ran, &mut reader, &mut out, owed).await? {
                    Settled::Goes => {}
                    Settled::Ended => return Ok(After::Ended),
                    Settled::Cut => {
                        drop(reader);
                        return stopped(client, &mut out, ctl, owed).await;
                    }
                    Settled::Close => {
                        drop(reader);
                        return close_at_cut(client, &mut out, owed).await;
                    }
                }
            }
        }
    }
    // The script is gone: its unread items are read past so the client can take the next
    // batch, and what no one read, a stop first, is kept for the next settle or close.
    if let Some(Lead::Error(e)) = handed {
        owed.keep(e);
    }
    let guard = reader.guard;
    let drained = capped(guard, owed, reader.drain()).await;
    if let Some(why) = guard.stopped() {
        owed.keep(why.error());
    }
    match drained {
        Ok((_, found)) => {
            if let Some(e) = found {
                owed.keep(e);
            }
            Ok(After::Gone)
        }
        Err(lost) => {
            if matches!(lost.0, ExecuteError::SessionClosed) {
                owed.keep_closed();
            }
            Err(lost)
        }
    }
}

/// What `Next` found after an item.
enum Ahead {
    Item(Lead),
    /// An error in the skipped item ended the script.
    Ended,
    /// The response has no more items.
    Done,
}

/// Reads past a later item handed out and never executed, then to the next item's opening
/// token. An error in the skipped item drains the response and is kept for the next settle.
async fn read_ahead(
    reader: &mut Reader<'_>,
    skipped: Option<Lead>,
    owed: &mut Owed,
) -> Result<Ahead, Lost> {
    let guard = reader.guard;
    let error = match skipped {
        Some(Lead::Set(_)) => capped(guard, owed, reader.skip_set()).await?,
        Some(Lead::Error(e)) => Some(e),
        Some(Lead::Count(_)) | None => None,
    };
    if let Some(e) = error {
        // Kept before the drain, so a drain past the cap still owes it first.
        owed.keep(e);
        capped(guard, owed, async { reader.drain().await.map(|_| None) }).await?;
        return Ok(Ahead::Ended);
    }
    Ok(match reader.lead().await? {
        Some(lead) => Ahead::Item(lead),
        None => Ahead::Done,
    })
}

/// How an item left its send.
enum Settled {
    /// The response goes on to its next item.
    Goes,
    /// The script ends here.
    Ended,
    /// A stop cut the item; `stopped` ends the send.
    Cut,
    /// A cut under `OnTruncate::Close`; `close_at_cut` stops the send and loses the client.
    Close,
}

/// After an item: an error drains the response and ends the script; a lost client is the
/// item's error, kept if its stream is gone, and closes the session.
async fn settle_item(
    ran: Result<bool, Lost>,
    reader: &mut Reader<'_>,
    out: &mut Out<'_>,
    owed: &mut Owed,
) -> Result<Settled, Lost> {
    if out.closing && ran.is_ok() {
        return Ok(Settled::Close);
    }
    if out.stopped() {
        return match ran {
            Ok(_) => Ok(Settled::Cut),
            Err(_) => {
                if let Some(why) = out.halt().await {
                    owed.keep(why.error());
                }
                Err(Lost(ExecuteError::SessionClosed))
            }
        };
    }
    match ran {
        Ok(true) => {
            let guard = reader.guard;
            // The item's error went out; later ones in the response are not reported.
            let drain = async { reader.drain().await.map(|_| None) };
            capped(guard, owed, drain).await.map(|_| Settled::Ended)
        }
        Ok(false) => Ok(Settled::Goes),
        Err(lost) => {
            out.fail(lost.0, owed).await;
            Err(Lost(ExecuteError::SessionClosed))
        }
    }
}

/// Ends a send whose item a stop cut: the rest of the response is discarded, then the item
/// answers the cause, or the next `Next` does when the item's outcome went out already.
async fn stopped(
    client: &mut MsClient,
    out: &mut Out<'_>,
    ctl: &mut mpsc::UnboundedReceiver<Ctl>,
    unread: &mut Owed,
) -> Result<After, Lost> {
    let drained = drain_stopped(client, out.guard).await;
    let owed = out.halt().await;
    match (drained, owed) {
        (Ok(_), Some(why)) => halted(why, ctl, unread).await,
        (Ok(_), None) => Ok(After::Ended),
        (Err(lost), owed) => {
            if let Some(why) = owed {
                unread.keep(why.error());
            }
            Err(lost)
        }
    }
}

/// Ends a send at a cut under `OnTruncate::Close`: the rest of the response is stopped with
/// Attention and discarded through the acknowledgement. A stop that also cut the item still
/// answers it with its cause. The descriptor is read first, as the drain can discard its change.
async fn close_at_cut(
    client: &mut MsClient,
    out: &mut Out<'_>,
    owed: &mut Owed,
) -> Result<After, Lost> {
    let seen = client.transaction_descriptor();
    let drained = drain_stopped(client, out.guard).await.ok();
    if let Some(why) = out.halt().await {
        owed.keep(why.error());
    }
    Ok(After::Cut { seen, drained })
}

/// Discards the rest of a stopped send's response packet by packet through the Attention
/// acknowledgement, sending the Attention if no read has; past `ATTENTION_BOUND` from the
/// stop the client is lost. Returns whether the response was still being stopped, not whole.
async fn drain_stopped(client: &mut MsClient, guard: Guard<'_>) -> Result<bool, Lost> {
    match tokio::time::timeout_at(guard.deadline(), client.drain_attention()).await {
        Ok(Ok(attended)) => resync_transaction(client).await.map(|()| attended),
        _ => Err(Lost(ExecuteError::SessionClosed)),
    }
}

/// SQL Server's answer to a request naming a transaction that no longer exists.
const STALE_TRANSACTION: u32 = 3971;

/// A drain can discard the ENVCHANGE of a rollback Attention caused, leaving the client naming a
/// dead transaction on every request; only the server can tell, so a probe asks it.
async fn resync_transaction(client: &mut MsClient) -> Result<(), Lost> {
    let probe = async {
        let mut stream = client.simple_query_items("SELECT @@TRANCOUNT").await?;
        let mut stale = false;
        while let Some(item) = stream.try_next().await? {
            stale |= matches!(&item, QueryItem::Error(e) if e.code() == STALE_TRANSACTION);
        }
        Ok::<_, tiberius::error::Error>(stale)
    };
    match tokio::time::timeout(ATTENTION_BOUND, probe).await {
        Ok(Ok(stale)) => {
            if stale {
                client.forget_transaction();
            }
            Ok(())
        }
        _ => Err(Lost(ExecuteError::SessionClosed)),
    }
}

/// Once a stopped send's response is gone: answers the next `Next` or `Execute` with the
/// cause, or keeps it for the next settle or close if the script went away unread.
async fn halted(
    why: Stop,
    ctl: &mut mpsc::UnboundedReceiver<Ctl>,
    owed: &mut Owed,
) -> Result<After, Lost> {
    while let Some(msg) = ctl.recv().await {
        match msg {
            Ctl::IsLast { reply } => drop(reply.send(true)),
            Ctl::Next { reply } => {
                let _ = reply.send(Some(Err(why.error())));
                return Ok(After::Ended);
            }
            Ctl::Execute { reply, .. } => {
                let _ = reply.send(Err(why.error()));
                return Ok(After::Ended);
            }
        }
    }
    owed.keep(why.error());
    Ok(After::Gone)
}

/// The send a statement belongs to, for asking whether it was stopped.
#[derive(Clone, Copy)]
struct Guard<'a> {
    cancel: &'a MssqlCancel,
    g: u64,
}

impl Guard<'_> {
    fn stopped(&self) -> Option<Stop> {
        self.cancel.stopped(self.g)
    }

    /// When the stopped send's response must be gone; from now when no stop is recorded.
    fn deadline(&self) -> tokio::time::Instant {
        self.cancel
            .deadline(self.g)
            .unwrap_or_else(|| tokio::time::Instant::now() + ATTENTION_BOUND)
    }
}

/// Runs `work`, an await on the caller, unless the send is stopped first.
async fn unless_stopped<F: Future>(guard: Guard<'_>, work: F) -> Result<F::Output, Stop> {
    let stop = pin!(guard.cancel.until_stopped(guard.g));
    match select(pin!(work), stop).await {
        Either::Left((done, _)) => Ok(done),
        Either::Right((why, _)) => Err(why),
    }
}

/// Where one statement's answer goes: its `Execute` reply until the columns are known, then
/// its event channel until the outcome is sent; with the bounds it runs under and the send it
/// belongs to. Once a stop cuts the statement nothing more goes out but the cause, from `halt`.
struct Out<'a> {
    reply: Option<oneshot::Sender<Result<Vec<ColumnInfo>, ExecuteError>>>,
    events: Option<&'a mpsc::Sender<Event>>,
    limits: Limits,
    resume: Option<mpsc::UnboundedReceiver<Option<u64>>>,
    guard: Guard<'a>,
    stop: Option<Stop>,
    /// Set at a cut under `OnTruncate::Close`, which ends the session.
    closing: bool,
}

impl<'a> Out<'a> {
    fn new(
        reply: oneshot::Sender<Result<Vec<ColumnInfo>, ExecuteError>>,
        events: &'a mpsc::Sender<Event>,
        limits: Limits,
        resume: Option<mpsc::UnboundedReceiver<Option<u64>>>,
        guard: Guard<'a>,
    ) -> Self {
        Out {
            reply: Some(reply),
            events: Some(events),
            limits,
            resume,
            guard,
            stop: None,
            closing: false,
        }
    }

    /// Whether a stop cut this statement short.
    fn stopped(&self) -> bool {
        self.stop.is_some()
    }

    fn columns(&mut self, columns: Vec<ColumnInfo>) {
        if self.stopped() {
            return;
        }
        if let Some(r) = self.reply.take() {
            let _ = r.send(Ok(columns));
        }
    }

    /// Reports `e` as the statement's error; one its caller cannot receive, because the
    /// outcome went out already or the stream is gone, is kept instead. A stopped send
    /// reports its cause instead, through `halt`.
    async fn fail(&mut self, e: ExecuteError, owed: &mut Owed) {
        owed.send_failed = true;
        if self.stopped() {
            return;
        }
        let undelivered = match (self.reply.take(), self.events.take()) {
            (Some(r), _) => r.send(Err(e)).err().and_then(Result::err),
            (None, Some(events)) => match events.send(Event::Failed(e)).await {
                Err(mpsc::error::SendError(Event::Failed(e))) => Some(e),
                _ => None,
            },
            (None, None) => Some(e),
        };
        if let Some(e) = undelivered {
            owed.keep(e);
        }
    }

    /// Answers the stopped statement with its cause; returns the cause when the outcome went
    /// out already, so the next `Next` owes it. A stream dropped since takes it as delivered.
    async fn halt(&mut self) -> Option<Stop> {
        let why = self.stop?;
        match (self.reply.take(), self.events.take()) {
            (Some(r), _) => drop(r.send(Err(why.error()))),
            (None, Some(events)) => drop(events.send(Event::Failed(why.error())).await),
            (None, None) => return Some(why),
        }
        None
    }

    /// Sends the outcome; nothing follows it on this stream.
    async fn done(&mut self, outcome: Outcome) {
        if self.stopped() {
            return;
        }
        if let Some(events) = self.events.take() {
            let _ = events.send(Event::Done(outcome)).await;
        }
    }

    /// Whether the stream still listens and the send goes on; a stop ends the wait for the
    /// caller, so the response can be read on through the acknowledgement.
    async fn send(&mut self, event: Event) -> bool {
        // Rows only go out mid-set, and a stopped send's open set is cut.
        if self.stop.is_none() {
            self.stop = self.guard.stopped();
        }
        if self.stopped() {
            return false;
        }
        let Some(events) = self.events else {
            return false;
        };
        match unless_stopped(self.guard, events.send(event)).await {
            Ok(sent) => sent.is_ok(),
            Err(why) => {
                self.stop = Some(why);
                false
            }
        }
    }
}

/// The result set an item is reading: which cells are numbers, and the rows so far.
struct Open {
    numbers: Vec<bool>,
    builder: BatchBuilder,
    scratch: Vec<String>,
    rows: u64,
}

impl Open {
    /// The set's columns and an empty reader for its rows; a type with no name fails by name.
    fn new(meta: &ResultMetadata) -> Result<(Vec<ColumnInfo>, Open), ExecuteError> {
        let mut infos = Vec::new();
        let mut numbers = Vec::new();
        for column in meta.columns() {
            let kind = column
                .type_info()
                .and_then(|ty| wire_type(ty, column.user_type()));
            let Some((name, number)) = kind else {
                return Err(ExecuteError::Unsupported {
                    reason: format!(
                        "column {} has SQL Server type {:?}, which Dbento cannot show yet",
                        column.name(),
                        column.column_type()
                    ),
                });
            };
            infos.push(ColumnInfo::new(column.name(), name, None));
            numbers.push(number);
        }
        let set = Open {
            builder: BatchBuilder::new(numbers.len()),
            scratch: vec![String::new(); numbers.len()],
            numbers,
            rows: 0,
        };
        Ok((infos, set))
    }

    /// Appends `row`, returning the batches it closed.
    fn push(&mut self, row: &Row) -> Result<Vec<ResultBatch>, ExecuteError> {
        let mut cells = Vec::with_capacity(self.numbers.len());
        for ((column, data), buf) in row.cells().zip(self.scratch.iter_mut()) {
            cells.push(render(column.name(), data, buf)?);
        }
        let numbers = &self.numbers;
        let closed = self.builder.push_typed_row(&cells, |i| numbers[i]);
        self.rows += 1;
        let full = self.builder.is_full().then(|| self.builder.finish());
        Ok(closed.into_iter().chain(full).collect())
    }
}

/// Delivers one item from its opening token: a result set, a row count, an error, or (for
/// `None`) the one empty result of a response with no item. Under `last` the rest of the
/// response is drained and its result sets counted. Returns whether the script ends here.
async fn run_item(
    reader: &mut Reader<'_>,
    lead: Option<Lead>,
    last: bool,
    out: &mut Out<'_>,
    owed: &mut Owed,
) -> Result<bool, Lost> {
    let mut outcome = Outcome {
        rows: 0,
        truncated: false,
        rows_affected: None,
        truncated_by: None,
        extra_results: 0,
        rolled_back: false,
    };
    match lead {
        // A stopped send that yields no item was cut before its first.
        None => out.stop = out.guard.stopped(),
        Some(Lead::Error(e)) => {
            out.fail(e, owed).await;
            return Ok(true);
        }
        Some(Lead::Count(n)) => outcome.rows_affected = Some(n),
        Some(Lead::Set(meta)) => {
            let (columns, mut set) = match Open::new(&meta) {
                Ok(opened) => opened,
                Err(e) => {
                    out.fail(e, owed).await;
                    return Ok(true);
                }
            };
            out.columns(columns);
            loop {
                match reader.next().await? {
                    Some(Tok::Row(row)) => {
                        // This row proves a bound was reached. A row cut under Park waits with
                        // it held for more; any other cut ends the item before it.
                        let mut cut = out.limits.cut(set.rows, set.builder.total_bytes());
                        while cut == Some(TruncatedBy::Rows) && out.resume.is_some() {
                            if !park(&mut set, out).await {
                                return abandon(reader, true, last, out, owed).await;
                            }
                            cut = out.limits.cut(set.rows, set.builder.total_bytes());
                        }
                        if let Some(by) = cut {
                            return cut_item(reader, set, by, last, out, owed).await;
                        }
                        let batches = match set.push(&row) {
                            Ok(b) => b,
                            Err(e) => {
                                out.fail(e, owed).await;
                                return Ok(true);
                            }
                        };
                        for batch in batches {
                            if !out.send(Event::Batch(batch)).await {
                                return abandon(reader, true, last, out, owed).await;
                            }
                        }
                    }
                    Some(Tok::Done(d)) if d.kind == DoneKind::DoneProc => {}
                    // Attention closes the statement it cut with an error DONE, then
                    // acknowledges in a DONE of its own.
                    Some(Tok::Done(d))
                        if d.attention || (d.error && out.guard.stopped().is_some()) =>
                    {
                        out.stop = Some(out.guard.stopped().unwrap_or(Stop::Cancelled));
                        break;
                    }
                    Some(Tok::Done(_)) | None => break,
                    Some(Tok::Meta(m)) => {
                        reader.held = Some(Tok::Meta(m));
                        break;
                    }
                    Some(Tok::Error(e)) => {
                        out.fail(e, owed).await;
                        return Ok(true);
                    }
                }
            }
            outcome.rows = set.rows;
            if !set.builder.is_empty() && !out.send(Event::Batch(set.builder.finish())).await {
                return abandon(reader, false, last, out, owed).await;
            }
        }
    }
    out.columns(Vec::new());
    let mut error = None;
    if last {
        let guard = reader.guard;
        let (sets, e) = capped(guard, owed, reader.drain()).await?;
        outcome.extra_results = sets;
        error = e;
    }
    match error {
        Some(e) => out.fail(e, owed).await,
        None => out.done(outcome).await,
    }
    Ok(false)
}

/// The outcome of a result cut after `rows` rows.
fn truncated(rows: u64, by: TruncatedBy) -> Outcome {
    Outcome {
        rows,
        truncated: true,
        rows_affected: None,
        truncated_by: Some(by),
        extra_results: 0,
        rolled_back: false,
    }
}

/// Parks at a row cut: hands over the rows so far and the cut, then waits for `extend`.
/// `Some(n)` raises the row bound by n and `None` lifts it; false means the stream is gone.
async fn park(set: &mut Open, out: &mut Out<'_>) -> bool {
    if !set.builder.is_empty() && !out.send(Event::Batch(set.builder.finish())).await {
        return false;
    }
    if !out
        .send(Event::Parked(truncated(set.rows, TruncatedBy::Rows)))
        .await
    {
        return false;
    }
    let guard = out.guard;
    let more = match out.resume.as_mut() {
        Some(resume) => match unless_stopped(guard, resume.recv()).await {
            Ok(more) => more,
            Err(why) => {
                out.stop = Some(why);
                None
            }
        },
        None => None,
    };
    let Some(more) = more else {
        return false;
    };
    out.limits.max_rows = more.and_then(|m| out.limits.max_rows.map(|r| r.saturating_add(m)));
    true
}

/// Ends an item at a cut: the rows so far and the truncated outcome go out, then `Close`
/// leaves the send to `close_at_cut` and any other mode drains the rest.
async fn cut_item(
    reader: &mut Reader<'_>,
    mut set: Open,
    by: TruncatedBy,
    last: bool,
    out: &mut Out<'_>,
    owed: &mut Owed,
) -> Result<bool, Lost> {
    if set.builder.is_empty() || out.send(Event::Batch(set.builder.finish())).await {
        out.done(truncated(set.rows, by)).await;
    }
    if out.limits.on_truncate == OnTruncate::Close {
        out.closing = true;
        return Ok(true);
    }
    abandon(reader, true, last, out, owed).await
}

/// The rest of the item (under `last`, of the response) is read past within `DRAIN_CAP`, and
/// an error in it kept for the next settle or close; that error ends the script. A stopped
/// send is left to `settle_item`, which reads it through the acknowledgement.
async fn abandon(
    reader: &mut Reader<'_>,
    mid_set: bool,
    last: bool,
    out: &mut Out<'_>,
    owed: &mut Owed,
) -> Result<bool, Lost> {
    if out.stopped() {
        return Ok(true);
    }
    let guard = reader.guard;
    let error = capped(guard, owed, async {
        let mut error = if mid_set {
            reader.skip_set().await?
        } else {
            None
        };
        if last && error.is_none() {
            error = reader.drain().await?.1;
        }
        Ok(error)
    })
    .await?;
    Ok(match error {
        Some(e) => {
            owed.keep(e);
            true
        }
        None => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The live lock test can only bound the wait loosely, because the server sometimes gives up
    /// early, so the exact value is pinned here.
    #[test]
    fn a_catalog_call_waits_five_seconds_for_a_lock() {
        assert!(CATALOG_SETTINGS.starts_with("SET LOCK_TIMEOUT 5000;"));
    }

    #[test]
    fn owed_errors_come_out_oldest_first_and_a_closing_error_is_never_lost() {
        let mut owed = Owed::default();
        owed.keep(ExecuteError::database(
            "8134",
            "Divide by zero error encountered.",
        ));
        owed.keep(ExecuteError::database("208", "Invalid object name 'x'."));
        owed.keep(ExecuteError::StoppedAtDrainCap);
        owed.keep(ExecuteError::SessionClosed);
        assert!(matches!(owed.take(), Some(ExecuteError::Database { code, .. }) if code == "8134"));
        assert!(matches!(owed.take(), Some(ExecuteError::StoppedAtDrainCap)));
        assert!(owed.take().is_none());
    }

    #[test]
    fn a_fence_verdict_outranks_an_older_unread_error() {
        let mut owed = Owed::default();
        owed.keep(ExecuteError::database("208", "Invalid object name 'x'."));
        owed.keep(ExecuteError::ReadOnlyViolation {
            message: LOW_COUNT.to_owned(),
        });
        assert!(matches!(
            owed.take(),
            Some(ExecuteError::ReadOnlyViolation { .. })
        ));
    }

    #[test]
    fn only_one_set_of_one_int_column_and_one_row_is_a_close_out() {
        assert_eq!(close_out_count(1, 1, &[Some(2)]), Some(2));
        assert_eq!(close_out_count(1, 1, &[Some(0)]), Some(0));
        for (sets, columns, cells) in [
            (0, 0, &[][..]),
            (1, 1, &[][..]),
            (1, 1, &[None][..]),
            (1, 1, &[Some(2), Some(2)][..]),
            (1, 2, &[Some(2)][..]),
            (2, 1, &[Some(2)][..]),
        ] {
            assert_eq!(
                close_out_count(sets, columns, cells),
                None,
                "{sets} {columns}"
            );
        }
    }
}
