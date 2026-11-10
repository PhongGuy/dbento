//! The MySQL and MariaDB driver over mysql_async's text protocol.
//!
//! The session's one `Conn` lives in a shared slot. The driver, its script and its stream
//! lend it to each other: a stream holds it while its rows are read and gives it back on drop,
//! with a row read still in flight if it has one, leaving any unread rest for the next request
//! to settle under `DRAIN_CAP`. A `Close` cut and the cap stop the statement with `KILL QUERY`
//! from a side connection; every connection the driver lets go of is severed within the cap.

pub(crate) mod catalog;

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};
use std::time::{Duration, Instant};

use futures_util::FutureExt;
use futures_util::future::{BoxFuture, Either};
use mysql_async::consts::{ColumnFlags, ColumnType};
use mysql_async::prelude::Queryable;
use mysql_async::{
    ClientIdentity, Column, Conn, DriverError, IoError, Opts, OptsBuilder, QueryResult, Row,
    SslOpts, TextProtocol, Value,
};
use pkcs8::der::pem::{self, LineEnding};
use rustls::pki_types::pem::PemObject as _;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};

use crate::Secret;
use crate::classify::{mysql_changes_settings, mysql_classification, mysql_counts_rows};
use crate::introspect::{ColumnMeta, IntrospectError, RelationList, RelationRef, SchemaInfo};
use crate::postgres::{TLS_WAYS_OUT, client_auth, tls_error};
use crate::session::{
    CancelHandle, ConnectError, ConnectParams, DRAIN_CAP, Driver, ExecuteError, ExtendError,
    Flavor, Limits, OnTruncate, Outcome, Prepared, ServerInfo, StreamItem, TruncatedBy,
};
use crate::value::{BatchBuilder, CellRef, ColumnInfo, ColumnSource, ResultBatch};
use crate::workspace::{ConnectionConfig, Engine, TlsMode};

/// Bounds the TCP connect, the handshake and the version query together.
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// Probes an idle connection this often, so a silent network drop ends the session.
const KEEPALIVE: Duration = Duration::from_secs(30);
/// Bounds the side connection's connect, `KILL QUERY` and disconnect together.
const KILL_TIMEOUT: Duration = Duration::from_secs(10);

/// One MySQL session: the shared connection slot and what the handshake reported.
pub(crate) struct MysqlDriver {
    shared: Shared,
    server: ServerInfo,
    cancel: Arc<MysqlCancel>,
    door: Arc<KillDoor>,
    /// Set once a SET or USE is sent on this session; see `Session::settings_changed`.
    settings_changed: Arc<AtomicBool>,
    /// net_write_timeout was raised since connect or the last user SET.
    net_write_raised: Arc<AtomicBool>,
    /// The effective statement timeout, enforced by the watchdog.
    timeout: Option<Duration>,
    /// Every statement runs in its own read-only transaction.
    read_only: bool,
    /// `@@lower_case_table_names` was 1 or 2 at connect, so the server folds names.
    fold_names: bool,
    /// `DATABASE()` at connect: the configured database, or None without one.
    opened_on: Option<String>,
}

/// One script: the trimmed statement texts not yet prepared and the one prepared last.
pub(crate) struct MysqlScript {
    shared: Shared,
    pieces: std::vec::IntoIter<String>,
    current: Option<String>,
    ended: bool,
    door: Arc<KillDoor>,
    cancel: Arc<MysqlCancel>,
    settings_changed: Arc<AtomicBool>,
    net_write_raised: Arc<AtomicBool>,
    timeout: Option<Duration>,
    /// Time the current statement's prepare check took, taken from its execute's timeout.
    prepare_spent: Duration,
    read_only: bool,
    server: ServerInfo,
}

/// A running statement, holding the session's connection until it is dropped.
pub(crate) struct MysqlStream {
    shared: Shared,
    /// Lent from the slot for the stream's life; `None` once a fatal error dropped it.
    held: Option<Held>,
    /// The first result set's columns.
    columns: Arc<[Column]>,
    /// Per column: whether its cells are always bytes, and whether they are numbers.
    kinds: Vec<(bool, bool)>,
    builder: BatchBuilder,
    delivered: u64,
    limits: Limits,
    /// The row that proved a parked row cut; it is delivered first once `extend` raises the cap.
    peeked: Option<Row>,
    door: Arc<KillDoor>,
    cancel: Arc<MysqlCancel>,
    /// The statement's generation in `cancel`.
    g: u64,
    /// Set once the stream knows how it ended.
    done: Option<Outcome>,
    /// The first result set was read to its end or failed.
    finished: bool,
    /// The result was cut or dropped before its end; drained by the next request.
    unread: bool,
}

/// Why Dbento stopped a statement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Stop {
    Cancelled,
    Timeout,
}

impl Stop {
    fn error(self) -> ExecuteError {
        match self {
            Stop::Cancelled => ExecuteError::Cancelled,
            Stop::Timeout => ExecuteError::Timeout,
        }
    }
}

/// The statement a stop may aim at. Never locked across an await.
#[derive(Default)]
struct Target {
    /// From just before Dbento sends the statement until its result is read or settled.
    running: Option<u64>,
    /// The last generation handed out; the first is 1.
    last: u64,
    /// The first stop sent, with its generation.
    stop: Option<(u64, Stop)>,
    /// A cancel that found nothing running; the next statement is not sent.
    requested: bool,
    /// The running statement's timeout, aborted once it ends or settles.
    watchdog: Option<tokio::task::AbortHandle>,
    /// The generation the watchdog guards and when it fires.
    deadline: Option<(u64, tokio::time::Instant)>,
}

impl Target {
    /// Nothing runs any more, so its watchdog goes too.
    fn idle(&mut self) {
        self.running = None;
        self.deadline = None;
        if let Some(watchdog) = self.watchdog.take() {
            watchdog.abort();
        }
    }
}

/// Stops the session's running statement by generation, with `KILL QUERY` from a side
/// connection, so a kill that arrives late never lands on the statement after it.
pub(crate) struct MysqlCancel {
    door: Arc<KillDoor>,
    target: Arc<Mutex<Target>>,
    /// One per KILL sent and not yet known to be finished.
    inflight: Mutex<Vec<tokio::sync::oneshot::Receiver<()>>>,
    /// The last generation whose KILL could not reach the server.
    abandoned: tokio::sync::watch::Sender<u64>,
    /// The thread that sends KILLs, started by the first stop; the cause when it could not start.
    killer: OnceLock<Result<tokio::sync::mpsc::UnboundedSender<KillJob>, String>>,
}

/// One KILL for the killer thread: the statement's generation, and where to say it finished.
type KillJob = (u64, tokio::sync::oneshot::Sender<()>);

impl std::fmt::Debug for MysqlCancel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MysqlCancel").finish_non_exhaustive()
    }
}

impl MysqlCancel {
    pub(crate) fn new(door: Arc<KillDoor>) -> Self {
        MysqlCancel {
            door,
            target: Arc::default(),
            inflight: Mutex::default(),
            abandoned: tokio::sync::watch::Sender::new(0),
            killer: OnceLock::new(),
        }
    }

    pub(crate) fn cancel(&self) {
        let running = {
            let mut t = lock(&self.target);
            t.requested |= t.running.is_none();
            t.running
        };
        if let Some(g) = running {
            self.stop(g, Stop::Cancelled);
        }
    }

    /// Records `why` for `g` unless it has a stop, then hands the KILL QUERY to the killer
    /// thread; the side connection rechecks that `g` still runs right before it sends.
    fn stop(&self, g: u64, why: Stop) {
        {
            let mut t = lock(&self.target);
            if t.running != Some(g) || t.stop.is_some_and(|(s, _)| s == g) {
                return;
            }
            t.stop = Some((g, why));
        }
        // In flight before it is sent, so a `spend()` that runs meanwhile still waits for it;
        // a send that fails drops `done`, and the receiver resolves at once.
        let (done, finished) = tokio::sync::oneshot::channel();
        lock(&self.inflight).push(finished);
        let queued = match self.killer.get_or_init(|| self.start_killer()) {
            Ok(killer) => killer
                .send((g, done))
                .map_err(|_| "the kill thread has stopped".to_owned()),
            Err(cause) => Err(cause.clone()),
        };
        if let Err(cause) = queued {
            abandon(&self.door, &self.abandoned, g, &cause);
        }
    }

    /// Starts the thread that sends KILLs on a runtime of its own, so a session runtime busy
    /// reading a hot result never starves the side connection. It ends with the last sender,
    /// once every KILL it took has finished.
    fn start_killer(&self) -> Result<tokio::sync::mpsc::UnboundedSender<KillJob>, String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|e| format!("cannot start the kill runtime: {e}"))?;
        let (jobs, mut rx) = tokio::sync::mpsc::unbounded_channel::<KillJob>();
        let (door, target, abandoned) = (
            Arc::clone(&self.door),
            Arc::clone(&self.target),
            self.abandoned.clone(),
        );
        std::thread::Builder::new()
            .name("dbento-mysql-kill".into())
            .spawn(move || {
                runtime.block_on(async move {
                    let mut kills = tokio::task::JoinSet::new();
                    while let Some((g, done)) = rx.recv().await {
                        let (door, target, abandoned) =
                            (Arc::clone(&door), Arc::clone(&target), abandoned.clone());
                        kills.spawn(async move {
                            let still = move || lock(&target).running == Some(g);
                            if let Kill::Unreachable(cause) = door.kill_if(still).await {
                                abandon(&door, &abandoned, g, &cause);
                            }
                            let _ = done.send(());
                        });
                    }
                    while kills.join_next().await.is_some() {}
                });
            })
            .map_err(|e| format!("cannot start the kill thread: {e}"))?;
        Ok(jobs)
    }

    /// Waits out every KILL sent so far, so none of them races what the caller does next.
    async fn spend(&self) {
        let finished = std::mem::take(&mut *lock(&self.inflight));
        for kill in finished {
            let _ = kill.await;
        }
    }

    /// Waits out every KILL sent so far, then starts the next statement; `None` when a
    /// cancel came with nothing running.
    async fn begin(&self) -> Option<u64> {
        self.spend().await;
        let mut t = lock(&self.target);
        if std::mem::take(&mut t.requested) {
            return None;
        }
        t.last += 1;
        t.running = Some(t.last);
        Some(t.last)
    }

    /// Stops statement `g` with `Stop::Timeout` once `after` has passed, unless it ends first.
    /// Armed before the statement is sent, so its deadline precedes MariaDB's own.
    fn arm(self: &Arc<Self>, g: u64, after: Duration) {
        let deadline = tokio::time::Instant::now() + after;
        let this = Arc::clone(self);
        let task = tokio::spawn(async move {
            tokio::time::sleep_until(deadline).await;
            this.stop(g, Stop::Timeout);
        });
        let mut t = lock(&self.target);
        t.deadline = Some((g, deadline));
        if let Some(old) = t.watchdog.replace(task.abort_handle()) {
            old.abort();
        }
    }

    /// Resolves once the running statement's KILL could not reach the server; never when
    /// nothing runs.
    async fn running_abandoned(&self) {
        let running = lock(&self.target).running;
        match running {
            Some(g) => self.abandoned(g).await,
            None => std::future::pending().await,
        }
    }

    /// Resolves once statement `g`'s KILL could not reach the server; never otherwise.
    async fn abandoned(&self, g: u64) {
        let mut rx = self.abandoned.subscribe();
        if rx.wait_for(|a| *a == g).await.is_err() {
            std::future::pending::<()>().await;
        }
    }

    /// The running statement's result was read to its end or failed.
    fn end(&self) {
        lock(&self.target).idle();
    }

    /// A settle finished: nothing runs, and a cancel that found nothing running is spent.
    fn settled(&self) {
        let mut t = lock(&self.target);
        t.idle();
        t.requested = false;
    }

    /// The stop sent to the statement still running, the one whose result settle reads back.
    fn running_stop(&self) -> Option<Stop> {
        let t = lock(&self.target);
        t.stop
            .filter(|&(s, _)| t.running == Some(s))
            .map(|(_, why)| why)
    }

    /// The stop sent to statement `g`, if any.
    fn stopped(&self, g: u64) -> Option<Stop> {
        lock(&self.target)
            .stop
            .filter(|&(s, _)| s == g)
            .map(|(_, why)| why)
    }

    /// Ends statement `g`, whose result was read to its end; a stop sent to it is its error,
    /// and so is a passed deadline, since MariaDB's max_statement_time can end it with OK.
    fn finish(&self, g: u64) -> Result<(), ExecuteError> {
        let overdue = lock(&self.target)
            .deadline
            .is_some_and(|(s, at)| s == g && tokio::time::Instant::now() >= at);
        self.end();
        let stop = self.stopped(g).or(overdue.then_some(Stop::Timeout));
        stop.map_or(Ok(()), |stop| Err(stop.error()))
    }
}

/// Stops the session's running statement from a short-lived side connection.
pub(crate) struct KillDoor {
    opts: Opts,
    /// The session connection's server thread id.
    thread: u32,
    /// The connection's display target, for the line an unreachable kill writes.
    target: String,
}

/// How a `KillDoor` attempt ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Kill {
    /// `KILL QUERY` reached the server, which may have answered that nothing was running.
    Sent,
    /// The side connection opened, but `still` said the statement no longer needs it.
    Skipped,
    /// The side connection failed or `KILL_TIMEOUT` ran out first, with the cause.
    Unreachable(String),
}

impl KillDoor {
    /// Sends `KILL QUERY <thread>` from a fresh connection within `KILL_TIMEOUT`.
    pub(crate) async fn kill(&self) -> Kill {
        self.kill_if(|| true).await
    }

    /// Opens the side connection, then sends the kill only if `still()` holds, so a statement
    /// that ended while it connected is spared. A server error reply still counts as `Sent`.
    pub(crate) async fn kill_if(&self, still: impl Fn() -> bool + Send) -> Kill {
        let attempt = async {
            let side = match Conn::new(self.opts.clone()).await {
                Ok(side) => side,
                Err(e) => return Kill::Unreachable(e.to_string()),
            };
            let mut side = Guarded::new(side);
            let kill = if !still() {
                Kill::Skipped
            } else {
                let sql = format!("KILL QUERY {}", self.thread);
                let sent: mysql_async::Result<()> =
                    async { side.conn()?.query_drop(sql).await }.await;
                match sent {
                    Ok(()) | Err(mysql_async::Error::Server(_)) => Kill::Sent,
                    Err(e) => Kill::Unreachable(e.to_string()),
                }
            };
            // Dropping the guard severs the side connection in the background, so a stalled
            // COM_QUIT never turns a sent kill into an unreachable one.
            drop(side);
            kill
        };
        tokio::time::timeout(KILL_TIMEOUT, attempt)
            .await
            .unwrap_or_else(|_| {
                Kill::Unreachable(format!("no answer within {} s", KILL_TIMEOUT.as_secs()))
            })
    }
}

/// Statement `g`'s KILL could not reach the server: says why on stderr and wakes its waiters.
fn abandon(door: &KillDoor, abandoned: &tokio::sync::watch::Sender<u64>, g: u64, cause: &str) {
    eprintln!(
        "dbento: cannot reach {} to stop the statement: {cause}",
        door.target
    );
    abandoned.send_replace(g);
}

/// Kills the running statement, then drops `held`, which severs its connection. Both run in a
/// task of their own, so a caller dropped mid-await never leaves a statement running unkilled.
async fn kill_then_sever(door: &Arc<KillDoor>, held: Held) {
    let door = Arc::clone(door);
    let _ = tokio::spawn(async move {
        door.kill().await;
        drop(held);
    })
    .await;
}

/// A connection the driver owns; dropped while it still holds one, it severs it.
struct Guarded(Option<Conn>);

impl Guarded {
    fn new(conn: Conn) -> Self {
        Guarded(Some(conn))
    }

    /// Empty only after `release`, which consumes the guard.
    fn conn(&mut self) -> mysql_async::Result<&mut Conn> {
        self.0
            .as_mut()
            .ok_or_else(|| mysql_async::DriverError::ConnectionClosed.into())
    }

    fn release(mut self) -> Option<Conn> {
        self.0.take()
    }
}

impl Drop for Guarded {
    fn drop(&mut self) {
        if let Some(conn) = self.0.take() {
            sever(conn);
        }
    }
}

/// Disconnects `conn` in the background within DRAIN_CAP; the cap dropping `disconnect`
/// mid-drain just closes the socket, since it marked the connection disconnected first.
fn sever(conn: Conn) {
    if let Ok(runtime) = tokio::runtime::Handle::try_current() {
        runtime.spawn(async move {
            let _ = tokio::time::timeout(DRAIN_CAP, conn.disconnect()).await;
        });
    }
}

/// A row read that owns the connection while it runs.
type ReadFut = BoxFuture<'static, (Guarded, mysql_async::Result<Option<Row>>)>;

/// The drain of the result sets after the current one, capped at DRAIN_CAP from its first
/// poll; its result is `None` once the cap cut it, and the connection then needs a kill.
type DrainFut = BoxFuture<'static, (Guarded, Option<mysql_async::Result<u32>>)>;

/// The connection at rest, or inside a row read or a drain still in flight. Every await
/// polls the future where it is stored, so a caller dropped mid-await loses nothing.
enum Held {
    Idle(Guarded),
    Reading(ReadFut),
    Draining(DrainFut),
}

/// What a finished read or drain produced.
enum Step {
    Read((Guarded, mysql_async::Result<Option<Row>>)),
    Drained((Guarded, Option<mysql_async::Result<u32>>)),
}

/// The session's connection while no stream holds it, and what the last drain left behind.
/// Dropped with the session, its held connection severs through `Guarded`.
#[derive(Default)]
struct Slot {
    held: Option<Held>,
    /// The connection has a pending result that the next request must drain first.
    unread: bool,
    /// A drain error not yet reported to the caller.
    unreported: Option<ExecuteError>,
    /// What is left of the drain cap after settles cut short; `None` is the full `DRAIN_CAP`.
    settle_budget: Option<Duration>,
    /// The pending result's stream already returned its stop, so the drain's error is not
    /// reported again.
    stop_reported: bool,
    /// A read-only statement's transaction is open; settle ends it once nothing is unread.
    owed_rollback: bool,
}

/// Never locked across an await.
type Shared = Arc<Mutex<Slot>>;

/// A catalog query that owns the connection while it runs.
type CatalogFut = BoxFuture<'static, (Guarded, mysql_async::Result<Vec<Row>>)>;

/// A catalog query in flight as statement `g`. Dropped before it finished, as an MCP cancel
/// drops it, it parks the query in the slot as a drain and kills it, so the next request's
/// settle reads back the interruption instead of the session being severed.
struct Asked<'a> {
    shared: &'a Shared,
    cancel: &'a MysqlCancel,
    g: u64,
    fut: Option<CatalogFut>,
}

impl Drop for Asked<'_> {
    fn drop(&mut self) {
        let Some(fut) = self.fut.take() else {
            return;
        };
        // Nobody waits for this answer, so only a fatal error, which closes the session, is kept.
        let drain: DrainFut = Box::pin(fut.map(|(held, rows)| {
            let ended = match rows {
                Err(e) if e.is_fatal() => Err(e),
                _ => Ok(0),
            };
            (held, Some(ended))
        }));
        lock(self.shared).held = Some(Held::Draining(drain));
        self.cancel.stop(self.g, Stop::Cancelled);
    }
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Drop for MysqlStream {
    fn drop(&mut self) {
        let mut slot = lock(&self.shared);
        if let Some(held) = self.held.take() {
            slot.held = Some(held);
        }
        slot.unread |= self.unread || !self.finished;
    }
}

impl MysqlDriver {
    pub(crate) fn server(&self) -> ServerInfo {
        self.server
    }

    pub(crate) fn settings_changed(&self) -> bool {
        self.settings_changed.load(Ordering::SeqCst)
    }

    /// Runs Dbento's own catalog text on the session's connection after settling it, as every
    /// request does; rows come from the text protocol, so every cell is bytes or NULL. It is a
    /// statement of its own generation, so the timeout and cancel stop it as they stop a query.
    pub(crate) async fn catalog(&mut self, sql: &str) -> Result<Vec<Row>, IntrospectError> {
        // An earlier statement's drain error stays that statement's; only a lost session counts.
        if let Err(e) = settle(&self.shared, &self.door, &self.cancel).await
            && e.closes_session()
        {
            return Err(ExecuteError::SessionClosed.into());
        }
        let cancel = &self.cancel;
        let Some(g) = cancel.begin().await else {
            return Err(ExecuteError::Cancelled.into());
        };
        // The watchdog bounds it on both flavors: neither MySQL's max_execution_time nor
        // MariaDB's max_statement_time ends a metadata-lock wait.
        if let Some(after) = self.timeout {
            cancel.arm(g, after);
        }
        let mut held = take_idle(&self.shared).inspect_err(|_| cancel.end())?;
        let sql = sql.to_owned();
        let mut asked = Asked {
            shared: &self.shared,
            cancel,
            g,
            fut: Some(Box::pin(async move {
                let rows = match held.conn() {
                    Ok(conn) => conn.query::<Row, _>(sql).await,
                    Err(e) => Err(e),
                };
                (held, rows)
            })),
        };
        let Some(fut) = asked.fut.as_mut() else {
            return Err(ExecuteError::SessionClosed.into());
        };
        let answered = unless_abandoned(cancel, g, fut).await;
        // Finished or abandoned, the query is no longer the slot's to keep.
        asked.fut = None;
        let Some((held, rows)) = answered else {
            return Err(abandoned_error(cancel, g).into());
        };
        match rows {
            Ok(rows) => {
                lock(&self.shared).held = Some(Held::Idle(held));
                // A stop that raced a query which then finished is still its error.
                cancel.finish(g)?;
                Ok(rows)
            }
            // A server error such as a table dropped meanwhile keeps the session; a fatal one
            // drops the guard, severing the connection and leaving the slot empty.
            Err(e) if !e.is_fatal() => {
                lock(&self.shared).held = Some(Held::Idle(held));
                let err = execute_error(&e, cancel.stopped(g));
                cancel.end();
                Err(err.into())
            }
            Err(_) => {
                cancel.end();
                Err(ExecuteError::SessionClosed.into())
            }
        }
    }

    /// Whether `@@lower_case_table_names` was 1 or 2 at connect.
    pub(crate) fn fold_names(&self) -> bool {
        self.fold_names
    }

    /// `DATABASE()` once a USE moved the session off the one it opened on; None on failure.
    pub(crate) async fn moved_database(&mut self) -> Option<String> {
        let rows = self.catalog("SELECT DATABASE()").await.ok()?;
        let now = catalog::cell(rows.first()?, 0).ok()??;
        (Some(&now) != self.opened_on.as_ref()).then_some(now)
    }
}

impl Driver for MysqlDriver {
    type Script = MysqlScript;
    type Stream = MysqlStream;

    async fn connect(params: ConnectParams<'_>) -> Result<Self, ConnectError> {
        let conn = params.conn;
        // Every file is read and checked here, before any I/O; side connections reuse `opts`.
        let ssl = match conn.tls() {
            Some(TlsMode::Verify) => Some(ssl_opts(conn, params.key_passphrase)?),
            _ => None,
        };
        let opts: Opts = OptsBuilder::default()
            .ip_or_hostname(conn.host().unwrap_or("localhost"))
            .tcp_port(conn.port().unwrap_or(3306))
            .user(conn.user())
            .pass(params.secret.map(|s| s.expose()))
            .db_name(conn.database())
            .prefer_socket(false)
            .client_found_rows(true)
            .tcp_keepalive(Some(KEEPALIVE))
            .init(vec!["SET NAMES utf8mb4"])
            .ssl_opts(ssl)
            .into();
        let handshake = async {
            let mut c = Conn::new(opts.clone())
                .await
                .map_err(|e| connect_error(conn, &e))?;
            let (version, lower_case, opened_on): (String, u8, Option<String>) = c
                .query_first("SELECT VERSION(), @@lower_case_table_names, DATABASE()")
                .await
                .map_err(|e| connect_error(conn, &e))?
                .unwrap_or_default();
            // MariaDB names itself in VERSION(); Oracle MySQL never does.
            let flavor = if version.contains("MariaDB") {
                Flavor::Mariadb
            } else {
                Flavor::Mysql
            };
            let server = ServerInfo {
                flavor,
                version: c.server_version(),
            };
            if let Some(sets) = connect_sets(server, params.read_only, params.timeout) {
                c.query_drop(sets)
                    .await
                    .map_err(|e| connect_error(conn, &e))?;
            }
            Ok::<_, ConnectError>((c, server, lower_case != 0, opened_on))
        };
        let (c, server, fold_names, opened_on) = tokio::time::timeout(CONNECT_TIMEOUT, handshake)
            .await
            .map_err(|_| ConnectError::Open {
                message: format!(
                    "{}: the server did not answer within {} s",
                    conn.display_target(),
                    CONNECT_TIMEOUT.as_secs()
                ),
            })??;
        let door = Arc::new(KillDoor {
            opts,
            thread: c.id(),
            target: conn.display_target(),
        });
        let shared = Arc::new(Mutex::new(Slot {
            held: Some(Held::Idle(Guarded::new(c))),
            ..Slot::default()
        }));
        Ok(MysqlDriver {
            shared,
            server,
            cancel: Arc::new(MysqlCancel::new(Arc::clone(&door))),
            door,
            settings_changed: Arc::default(),
            net_write_raised: Arc::default(),
            timeout: params.timeout,
            read_only: params.read_only,
            fold_names,
            opened_on,
        })
    }

    fn start_script(&mut self, sql: &str) -> MysqlScript {
        let pieces: Vec<String> = crate::ranges::statement_ranges(Engine::Mysql, sql)
            .into_iter()
            .map(|r| sql[r].trim().to_owned())
            .collect();
        MysqlScript {
            shared: Arc::clone(&self.shared),
            pieces: pieces.into_iter(),
            current: None,
            ended: false,
            door: Arc::clone(&self.door),
            cancel: Arc::clone(&self.cancel),
            settings_changed: Arc::clone(&self.settings_changed),
            net_write_raised: Arc::clone(&self.net_write_raised),
            timeout: self.timeout,
            prepare_spent: Duration::ZERO,
            read_only: self.read_only,
            server: self.server,
        }
    }

    async fn prepare_next(script: &mut MysqlScript) -> Option<Result<Prepared, ExecuteError>> {
        if script.ended {
            return None;
        }
        // Settle before reporting the end too, so a result cut at the last statement drains.
        if let Err(e) = settle(&script.shared, &script.door, &script.cancel).await {
            script.ended = true;
            return Some(Err(e));
        }
        let Some(sql) = script.pieces.next() else {
            script.ended = true;
            return None;
        };
        let classification = mysql_classification(&sql);
        if script.read_only {
            if classification.transaction_control {
                script.ended = true;
                return Some(Err(ExecuteError::TransactionControl {
                    reason: "transaction control is not available in a read-only session; \
                             each statement runs in its own read-only transaction"
                        .to_owned(),
                }));
            }
            // Prepare runs nothing and rejects multi-statement text the splitter could not
            // see into, so such text never reaches the server as a script.
            let started = Instant::now();
            if let Err(e) = prepare_check(script, &sql).await {
                script.ended = true;
                return Some(Err(e));
            }
            script.prepare_spent = started.elapsed();
        }
        script.current = Some(sql.clone());
        Some(Ok(Prepared {
            sql,
            classification,
        }))
    }

    async fn is_last(script: &mut MysqlScript) -> bool {
        script.pieces.len() == 0
    }

    async fn execute(
        script: &mut MysqlScript,
        limits: Limits,
    ) -> Result<(Vec<ColumnInfo>, MysqlStream), ExecuteError> {
        let sql = script.current.take().ok_or(ExecuteError::SessionClosed)?;
        let cancel = Arc::clone(&script.cancel);
        let Some(g) = cancel.begin().await else {
            return Err(ExecuteError::Cancelled);
        };
        // One timeout covers the prepare check and the execute; the wait between them is the
        // caller's, such as a confirmation prompt, and is not charged.
        let spent = std::mem::take(&mut script.prepare_spent);
        if let Some(after) = script.timeout {
            cancel.arm(g, after.saturating_sub(spent));
        }
        let mut held = take_idle(&script.shared).inspect_err(|_| cancel.end())?;
        let Ok(conn) = held.conn() else {
            cancel.end();
            return Err(ExecuteError::SessionClosed);
        };
        // A parked result may wait on the client for long, so the server must not time out
        // writing its next row: net_write_timeout is raised to an hour, once until a user SET.
        let raise = limits.on_truncate == OnTruncate::Park
            && limits.max_rows.is_some()
            && !script.net_write_raised.load(Ordering::SeqCst);
        let sets = statement_sets(script.server, script.read_only, script.timeout, raise);
        if let Some(sets) = sets {
            // Owed before it is sent, so any failure after this point still ends it.
            if script.read_only {
                lock(&script.shared).owed_rollback = true;
            }
            if let Err(e) = conn.query_drop(sets).await {
                return Err(failed(&script.shared, &script.door, &cancel, g, held, &e).await);
            }
            script.net_write_raised.fetch_or(raise, Ordering::SeqCst);
        }
        // Marked before sending: a SET or USE that then fails may still have taken effect.
        if mysql_changes_settings(&sql) {
            script.settings_changed.store(true, Ordering::SeqCst);
            script.net_write_raised.store(false, Ordering::SeqCst);
        }
        // The pending result lives in `Conn`, so the returned wrapper can go at once and
        // each later read opens a fresh one.
        let sent = async { conn.query_iter(sql.as_str()).await.map(drop) };
        match unless_abandoned(&cancel, g, sent).await {
            None => {
                drop(held);
                return Err(abandoned_error(&cancel, g));
            }
            Some(Err(e)) => {
                return Err(failed(&script.shared, &script.door, &cancel, g, held, &e).await);
            }
            Some(Ok(())) => {}
        }
        let columns = QueryResult::<'_, '_, TextProtocol>::new(&mut *conn)
            .columns()
            .unwrap_or_else(|| Arc::from(Vec::new()));
        // A first set without columns has nothing to stream, so the sets after it drain here
        // and their error is the statement's.
        let done = if columns.is_empty() {
            // Read before the drain, which moves the count on to the sets after this one.
            let affected = conn.affected_rows();
            let affected = (affected > 0 || mysql_counts_rows(&sql)).then_some(affected);
            let drained = tokio::time::timeout(DRAIN_CAP, drain_rest(conn));
            let Some(drained) = unless_abandoned(&cancel, g, drained).await else {
                drop(held);
                return Err(abandoned_error(&cancel, g));
            };
            match drained {
                Ok(Ok(extra)) => {
                    let finished = cancel.finish(g);
                    match (
                        finished,
                        end_read_only(&script.shared, &script.door, held).await,
                    ) {
                        (Ok(()), Ok(conn)) => held = conn,
                        (Err(stop), Ok(conn)) => {
                            lock(&script.shared).held = Some(Held::Idle(conn));
                            return Err(stop);
                        }
                        (finished, Err(e)) => return Err(stop_or(finished, e)),
                    }
                    Some(outcome(0, None, affected, extra))
                }
                Ok(Err(e)) => {
                    return Err(failed(&script.shared, &script.door, &cancel, g, held, &e).await);
                }
                Err(_) => {
                    kill_then_sever(&script.door, Held::Idle(held)).await;
                    return Err(ExecuteError::StoppedAtDrainCap);
                }
            }
        } else {
            None
        };
        let infos = columns
            .iter()
            .map(|c| ColumnInfo::new(c.name_str(), type_name(c), None).with_source(source(c)))
            .collect();
        let kinds = columns
            .iter()
            .map(|c| (always_bytes(c), c.column_type().is_numeric_type()))
            .collect();
        let stream = MysqlStream {
            shared: Arc::clone(&script.shared),
            held: Some(Held::Idle(held)),
            finished: done.is_some(),
            builder: BatchBuilder::new(columns.len()),
            columns,
            kinds,
            delivered: 0,
            limits,
            peeked: None,
            door: Arc::clone(&script.door),
            cancel,
            g,
            done,
            unread: false,
        };
        Ok((infos, stream))
    }

    async fn next_event(stream: &mut MysqlStream) -> Result<StreamItem, ExecuteError> {
        // A stopped statement hands out no more rows: the rest, and the server's 1317, are
        // left for the next settle to drain.
        if stream.done.is_none()
            && let Some(stop) = stream.cancel.stopped(stream.g)
        {
            stream.unread = true;
            lock(&stream.shared).stop_reported = true;
            return Err(stop.error());
        }
        while stream.done.is_none() {
            // A parked row goes first, checked against the raised limits as if it just arrived;
            // the connection is idle while one waits.
            let row = match stream.peeked.take() {
                Some(row) => row,
                None => match stream.next_row().await? {
                    Some(Ok(row)) => row,
                    Some(Err(batch)) => return Ok(StreamItem::Batch(batch)),
                    None => continue,
                },
            };
            // A row past a reached bound proves the result was cut short.
            let cut = stream
                .limits
                .cut(stream.delivered, stream.builder.total_bytes());
            match (cut, stream.limits.on_truncate) {
                (None, _) => {
                    if let Some(full) = stream.push(&row)? {
                        return Ok(StreamItem::Batch(full));
                    }
                    if stream.builder.is_full() {
                        return Ok(StreamItem::Batch(stream.builder.finish()));
                    }
                    continue;
                }
                // Under Park a row cut keeps that row and the open result for `extend`.
                (Some(TruncatedBy::Rows), OnTruncate::Park) => stream.peeked = Some(row),
                (Some(_), OnTruncate::Close) => {
                    if let Some(held) = stream.held.take() {
                        kill_then_sever(&stream.door, held).await;
                    }
                }
                (Some(_), _) => stream.unread = true,
            }
            stream.done = Some(outcome(stream.delivered, cut, None, 0));
        }
        if !stream.builder.is_empty() {
            return Ok(StreamItem::Batch(stream.builder.finish()));
        }
        let delivered = stream.delivered;
        Ok(StreamItem::Done(
            stream
                .done
                .clone()
                .unwrap_or_else(|| outcome(delivered, None, None, 0)),
        ))
    }

    fn extend(stream: &mut MysqlStream, more: Option<u64>) -> Result<(), ExtendError> {
        if stream.peeked.is_none() {
            return Err(ExtendError::NotParked);
        }
        stream.limits.max_rows =
            more.and_then(|m| stream.limits.max_rows.map(|r| r.saturating_add(m)));
        stream.done = None;
        Ok(())
    }

    fn cancel_handle(&self) -> CancelHandle {
        CancelHandle::mysql(Arc::clone(&self.cancel))
    }

    async fn settle(&mut self) -> Result<(), ExecuteError> {
        settle(&self.shared, &self.door, &self.cancel).await?;
        lock(&self.shared).unreported.take().map_or(Ok(()), Err)
    }

    async fn close(self) -> Result<(), ExecuteError> {
        let settled = settle(&self.shared, &self.door, &self.cancel).await;
        let (conn, unreported) = {
            let mut slot = lock(&self.shared);
            (slot.held.take(), slot.unreported.take())
        };
        // `settle` leaves the connection at rest whenever it leaves one at all.
        if let Some(conn) = conn.and_then(|h| match h {
            Held::Idle(g) => g.release(),
            Held::Reading(_) | Held::Draining(_) => None,
        }) {
            let _ = conn.disconnect().await;
        }
        settled?;
        unreported.map_or(Ok(()), Err)
    }

    async fn schemas(&mut self) -> Result<Vec<SchemaInfo>, IntrospectError> {
        catalog::schemas(self).await
    }

    async fn relations(&mut self, schema: &str) -> Result<RelationList, IntrospectError> {
        catalog::relations(self, schema).await
    }

    async fn columns(&mut self, rel: &RelationRef) -> Result<Vec<ColumnMeta>, IntrospectError> {
        catalog::columns(self, rel).await
    }
}

impl MysqlStream {
    /// Reads the next row off the connection: `Ok(row)` when one arrived, `Err(batch)` when a
    /// held batch closed on a read that was not ready, and `None` when the stream moved on
    /// without a row (into its drain, or to its outcome).
    async fn next_row(&mut self) -> Result<Option<Result<Row, ResultBatch>>, ExecuteError> {
        if let Some(Held::Idle(conn)) = self.held.take_if(|h| matches!(h, Held::Idle(_))) {
            self.held = Some(Held::Reading(read_row(conn)));
        }
        // Hand over what has arrived: once the batch holds a row, a read or drain that is
        // not ready closes the batch and stays in flight for the next call.
        let wait = self.builder.is_empty();
        let (cancel, g) = (&self.cancel, self.g);
        let step = match self.held.as_mut() {
            Some(Held::Reading(read)) => ready(read, wait, cancel, g)
                .await
                .map(|s| s.map(Step::Read)),
            Some(Held::Draining(drain)) => ready(drain, wait, cancel, g)
                .await
                .map(|s| s.map(Step::Drained)),
            // An idle connection became a read above, so only a dropped one lands here.
            Some(Held::Idle(_)) | None => return Err(ExecuteError::SessionClosed),
        };
        // Dropping the read severs its connection, which ends the statement and the session.
        let Ok(step) = step else {
            self.held = None;
            self.finished = true;
            return Err(abandoned_error(&self.cancel, self.g));
        };
        let Some(step) = step else {
            return Ok(Some(Err(self.builder.finish())));
        };
        self.held = None;
        match step {
            Step::Read((conn, Ok(Some(row)))) => {
                self.held = Some(Held::Idle(conn));
                return Ok(Some(Ok(row)));
            }
            Step::Read((conn, Ok(None))) => {
                self.finished = true;
                self.held = Some(Held::Draining(drain(conn)));
            }
            // A server error ends the response, so nothing is left to drain.
            Step::Read((conn, Err(e))) | Step::Drained((conn, Some(Err(e)))) if !e.is_fatal() => {
                self.held = Some(Held::Idle(conn));
                self.finished = true;
                self.cancel.end();
                let err = execute_error(&e, self.cancel.stopped(self.g));
                let _ = self.end_read_only().await;
                return Err(err);
            }
            Step::Drained((conn, Some(Ok(extra)))) => {
                self.held = Some(Held::Idle(conn));
                let finished = self.cancel.finish(self.g);
                if let Err(e) = self.end_read_only().await {
                    return Err(stop_or(finished, e));
                }
                finished?;
                self.done = Some(outcome(self.delivered, None, None, extra));
            }
            Step::Drained((conn, None)) => {
                kill_then_sever(&self.door, Held::Idle(conn)).await;
                return Err(ExecuteError::StoppedAtDrainCap);
            }
            // A fatal error leaves the connection unusable.
            Step::Read((_, Err(_))) | Step::Drained((_, Some(Err(_)))) => {
                return Err(ExecuteError::SessionClosed);
            }
        }
        Ok(None)
    }

    /// Ends the read-only transaction now the result is complete. The ROLLBACK is stored as
    /// a drain while it runs, so a caller dropped mid-await leaves it for the next settle.
    async fn end_read_only(&mut self) -> Result<(), ExecuteError> {
        if !std::mem::take(&mut lock(&self.shared).owed_rollback) {
            return Ok(());
        }
        if let Some(Held::Idle(conn)) = self.held.take_if(|h| matches!(h, Held::Idle(_))) {
            self.held = Some(Held::Draining(rollback(conn)));
        }
        let Some(Held::Draining(ending)) = self.held.as_mut() else {
            return Err(ExecuteError::SessionClosed);
        };
        let (conn, ended) = ending.await;
        self.held = None;
        let conn = rollback_ended(&self.shared, &self.door, conn, ended).await?;
        self.held = Some(Held::Idle(conn));
        Ok(())
    }

    /// Appends `row` to `builder`; returns a batch that `push_typed_row` closed. A cell is bytes
    /// when its column is binary or it is not UTF-8, else text; a cell that is not `Value::NULL`
    /// or `Value::Bytes` is `ExecuteError::Unsupported`, never NULL.
    fn push(&mut self, row: &Row) -> Result<Option<ResultBatch>, ExecuteError> {
        let mut cells = Vec::with_capacity(self.columns.len());
        for i in 0..row.len() {
            let binary = self.kinds.get(i).is_some_and(|k| k.0);
            cells.push(match row.as_ref(i) {
                Some(Value::NULL) => CellRef::Null,
                Some(Value::Bytes(b)) if binary => CellRef::Bytes(b),
                Some(Value::Bytes(b)) => match std::str::from_utf8(b) {
                    Ok(s) => CellRef::Text(s),
                    Err(_) => CellRef::Bytes(b),
                },
                _ => {
                    return Err(ExecuteError::Unsupported {
                        reason: format!(
                            "column {} arrived in a form the text protocol never sends",
                            i + 1
                        ),
                    });
                }
            });
        }
        self.delivered += 1;
        let kinds = &self.kinds;
        let number = |i: usize| kinds.get(i).is_some_and(|k| k.1) && cells[i] != CellRef::Null;
        Ok(self.builder.push_typed_row(&cells, number))
    }
}

/// The slot's contents lent to `settle`. Dropped mid-await, it puts them back, so a
/// cancelled settle leaves its read or drain for the next request.
/// It also carries the settle's drain budget and start, so a cut settle leaves the rest of the
/// budget to the next one; a finished settle clears `budget`, refilling the cap.
struct Lent<'a> {
    shared: &'a Shared,
    held: Option<Held>,
    unread: bool,
    budget: Option<(Duration, Instant)>,
}

impl Drop for Lent<'_> {
    fn drop(&mut self) {
        let mut slot = lock(self.shared);
        if let Some(held) = self.held.take() {
            slot.held = Some(held);
        }
        slot.unread |= self.unread;
        slot.settle_budget = self
            .budget
            .map(|(budget, start)| budget.saturating_sub(start.elapsed()));
    }
}

/// Finishes a row read a dropped stream left in flight, then reads any unread rest of the
/// pending result, all under DRAIN_CAP. A server error is kept for the caller; a fatal one
/// closes the session, and the cap kills the statement and closes it. Every request calls
/// this first, since mysql_async would otherwise drain a pending result with no cap.
async fn settle(
    shared: &Shared,
    door: &Arc<KillDoor>,
    cancel: &MysqlCancel,
) -> Result<(), ExecuteError> {
    // A KILL already sent lands before the drain starts, so the drain is not left to outrun it.
    cancel.spend().await;
    let mut lent = {
        let mut slot = lock(shared);
        let budget = slot.settle_budget.take().unwrap_or(DRAIN_CAP);
        Lent {
            shared,
            held: slot.held.take(),
            unread: std::mem::take(&mut slot.unread),
            budget: Some((budget, Instant::now())),
        }
    };
    let settled = settle_lent(&mut lent, door, cancel).await;
    lent.budget = None;
    lock(shared).stop_reported = false;
    cancel.settled();
    settled
}

/// The body of `settle`, which owns `lent`'s bookkeeping on every return path.
async fn settle_lent(
    lent: &mut Lent<'_>,
    door: &Arc<KillDoor>,
    cancel: &MysqlCancel,
) -> Result<(), ExecuteError> {
    let shared = lent.shared;
    let budget = lent.budget.map_or(DRAIN_CAP, |(b, _)| b);
    let deadline = tokio::time::Instant::now() + budget;
    loop {
        let step = match lent.held.as_mut() {
            None => return Err(ExecuteError::SessionClosed),
            Some(Held::Idle(_)) if !lent.unread => {
                // The read-only transaction ends once nothing is unread, as a drain the cap
                // bounds, so a settle cut short leaves it in flight for the next one.
                if !std::mem::take(&mut lock(shared).owed_rollback) {
                    return Ok(());
                }
                // Nothing runs now, so a deadline passing cannot KILL the ROLLBACK.
                cancel.end();
                if let Some(Held::Idle(conn)) = lent.held.take() {
                    lent.held = Some(Held::Draining(rollback(conn)));
                }
                continue;
            }
            Some(Held::Idle(_)) => {
                if let Some(Held::Idle(conn)) = lent.held.take() {
                    lent.held = Some(Held::Draining(drain(conn)));
                }
                lent.unread = false;
                continue;
            }
            Some(Held::Reading(read)) => capped(cancel, deadline, read)
                .await
                .map(|r| r.map(Step::Read)),
            Some(Held::Draining(drain)) => capped(cancel, deadline, drain)
                .await
                .map(|r| r.map(Step::Drained)),
        };
        // The stop's KILL cannot reach the server, so nothing ends the statement: dropping the
        // connection severs it, and the session is closed at once.
        let Ok(step) = step else {
            lent.held = None;
            let reported = std::mem::take(&mut lock(shared).stop_reported);
            return Err(match cancel.running_stop() {
                Some(stop) if !reported => stop.error(),
                _ => ExecuteError::SessionClosed,
            });
        };
        // The finished future, or the one the cap cut, which still holds the connection.
        let spent = lent.held.take();
        let (conn, failure) = match step {
            Ok(Step::Read((conn, Ok(_)))) => {
                lent.held = Some(Held::Draining(drain(conn)));
                continue;
            }
            Ok(Step::Read((conn, Err(e)))) | Ok(Step::Drained((conn, Some(Err(e))))) => {
                (conn, Some(e))
            }
            Ok(Step::Drained((conn, Some(Ok(_))))) => (conn, None),
            // Past the cap the statement is killed before its connection drops; the session
            // is closed.
            Ok(Step::Drained((conn, None))) => {
                kill_then_sever(door, Held::Idle(conn)).await;
                return Err(ExecuteError::StoppedAtDrainCap);
            }
            Err(_) => {
                if let Some(held) = spent {
                    kill_then_sever(door, held).await;
                }
                return Err(ExecuteError::StoppedAtDrainCap);
            }
        };
        match failure {
            Some(e) if e.is_fatal() => return Err(ExecuteError::SessionClosed),
            Some(e) => {
                let mut slot = lock(shared);
                if let Some(err) = drain_report(&e, slot.stop_reported, cancel.running_stop()) {
                    slot.unreported.get_or_insert(err);
                }
                drop(slot);
                // A server error ends the response; the drain after it reads nothing.
                lent.held = Some(Held::Draining(drain(conn)));
            }
            // At rest again; the loop's first arm ends a read-only transaction still owed.
            None => {
                lent.held = Some(Held::Idle(conn));
                continue;
            }
        }
    }
}

/// The error a settle's drain keeps for the caller; `None` when it is the interruption of a
/// stop the stream already reported.
fn drain_report(
    e: &mysql_async::Error,
    stop_reported: bool,
    stop: Option<Stop>,
) -> Option<ExecuteError> {
    (!(stop_reported && interrupted(e))).then(|| execute_error(e, stop))
}

/// Statement `g`'s KILL could not reach the server.
struct Abandoned;

/// Runs `fut` until `deadline`, or `Err` once the running statement's KILL is abandoned.
async fn capped<F: Future + Unpin>(
    cancel: &MysqlCancel,
    deadline: tokio::time::Instant,
    fut: &mut F,
) -> Result<Result<F::Output, tokio::time::error::Elapsed>, Abandoned> {
    let work = tokio::time::timeout_at(deadline, fut);
    let gone = std::pin::pin!(cancel.running_abandoned());
    match futures_util::future::select(std::pin::pin!(work), gone).await {
        Either::Left((out, _)) => Ok(out),
        Either::Right(_) => Err(Abandoned),
    }
}

/// Polls `fut` once, or when `wait` to its end unless `g`'s KILL is abandoned first;
/// `Ok(None)` when it was not ready.
async fn ready<F: Future + Unpin>(
    fut: &mut F,
    wait: bool,
    cancel: &MysqlCancel,
    g: u64,
) -> Result<Option<F::Output>, Abandoned> {
    if !wait {
        return Ok(fut.now_or_never());
    }
    unless_abandoned(cancel, g, fut)
        .await
        .map(Some)
        .ok_or(Abandoned)
}

/// Runs `work` to its end, or `None` once statement `g`'s KILL could not reach the server.
async fn unless_abandoned<F: Future>(cancel: &MysqlCancel, g: u64, work: F) -> Option<F::Output> {
    let work = std::pin::pin!(work);
    let gone = std::pin::pin!(cancel.abandoned(g));
    match futures_util::future::select(work, gone).await {
        Either::Left((out, _)) => Some(out),
        Either::Right(_) => None,
    }
}

/// The error of statement `g`, whose KILL could not reach the server; the caller dropped its
/// connection, which severs it, so the statement stops and the session is closed.
fn abandoned_error(cancel: &MysqlCancel, g: u64) -> ExecuteError {
    cancel.end();
    cancel.stopped(g).unwrap_or(Stop::Cancelled).error()
}

/// Drains the result sets after the current one under DRAIN_CAP, counting those with columns.
fn drain(mut held: Guarded) -> DrainFut {
    Box::pin(async move {
        let extra = match held.conn() {
            Ok(conn) => tokio::time::timeout(DRAIN_CAP, drain_rest(conn)).await.ok(),
            Err(e) => Some(Err(e)),
        };
        (held, extra)
    })
}

/// Ends the read-only transaction under DRAIN_CAP, shaped as a drain that read no result sets.
fn rollback(mut held: Guarded) -> DrainFut {
    Box::pin(async move {
        let ended = match held.conn() {
            Ok(conn) => tokio::time::timeout(DRAIN_CAP, conn.query_drop("ROLLBACK"))
                .await
                .ok()
                .map(|r| r.map(|()| 0)),
            Err(e) => Some(Err(e)),
        };
        (held, ended)
    })
}

/// Prepares `sql` and closes it at once, on the slot's connection at rest, as a statement of
/// its own generation: a pending DDL can hold the prepare, so cancel and the timeout stop it.
/// A server error is the statement's unless `prepare_refused` lets it run; a fatal one closes
/// the session.
async fn prepare_check(script: &MysqlScript, sql: &str) -> Result<(), ExecuteError> {
    let cancel = &script.cancel;
    let Some(g) = cancel.begin().await else {
        return Err(ExecuteError::Cancelled);
    };
    if let Some(after) = script.timeout {
        cancel.arm(g, after);
    }
    let mut held = take_idle(&script.shared).inspect_err(|_| cancel.end())?;
    let checked = async {
        let conn = held.conn()?;
        let stmt = conn.prep(sql).await?;
        conn.close(stmt).await
    };
    let Some(checked) = unless_abandoned(cancel, g, checked).await else {
        drop(held);
        return Err(abandoned_error(cancel, g));
    };
    let err = match checked {
        Ok(()) => None,
        Err(e) if e.is_fatal() => {
            cancel.end();
            return Err(ExecuteError::SessionClosed);
        }
        Err(mysql_async::Error::Server(s)) if !prepare_refused(s.code, sql) => None,
        Err(e) => Some(execute_error(&e, cancel.stopped(g))),
    };
    lock(&script.shared).held = Some(Held::Idle(held));
    // A stop that raced a prepare which then finished is still its error.
    let finished = cancel.finish(g);
    err.map_or(finished, Err)
}

/// Whether a prepare error refuses `sql`: every one does, except 1792 (a parsed write, which
/// the read-only transaction refuses at execute, so its classification is still seen) and
/// 1295 (the statement cannot be prepared) on text the splitter sees as exactly one statement.
fn prepare_refused(code: u16, sql: &str) -> bool {
    code != 1792 && (code != 1295 || crate::ranges::statement_ranges(Engine::Mysql, sql).len() != 1)
}

/// Takes the slot's connection at rest; `settle` has already finished any read or drain.
fn take_idle(shared: &Shared) -> Result<Guarded, ExecuteError> {
    match lock(shared).held.take() {
        Some(Held::Idle(conn)) => Ok(conn),
        // Unreachable after `settle`; dropping the future closes the session rather than guess.
        Some(Held::Reading(_) | Held::Draining(_)) | None => Err(ExecuteError::SessionClosed),
    }
}

/// Reads the next row of the pending result. The future owns the connection, so a caller
/// may poll it once, keep it, and finish it later without losing a packet.
fn read_row(mut held: Guarded) -> ReadFut {
    Box::pin(async move {
        let row = match held.conn() {
            Ok(conn) => QueryResult::<'_, '_, TextProtocol>::new(conn).next().await,
            Err(e) => Err(e),
        };
        (held, row)
    })
}

/// Counts the result sets after the first that have columns, reading each to its end.
async fn drain_rest(conn: &mut Conn) -> mysql_async::Result<u32> {
    let mut extra = 0;
    loop {
        let mut set = QueryResult::<'_, '_, TextProtocol>::new(&mut *conn);
        if set.is_empty() {
            // A later set's error waits as the pending result, which `is_empty` does not see;
            // one more read returns it, and reads nothing otherwise.
            set.next().await?;
            return Ok(extra);
        }
        if !set.columns_ref().is_empty() {
            extra += 1;
        }
        while set.next().await?.is_some() {}
    }
}

/// Statement `g` failed: a fatal error severs `conn`, closing the session; any other ends a
/// read-only transaction and returns it.
async fn failed(
    shared: &Shared,
    door: &Arc<KillDoor>,
    cancel: &MysqlCancel,
    g: u64,
    conn: Guarded,
    e: &mysql_async::Error,
) -> ExecuteError {
    cancel.end();
    let err = execute_error(e, cancel.stopped(g));
    if !e.is_fatal()
        && let Ok(conn) = end_read_only(shared, door, conn).await
    {
        lock(shared).held = Some(Held::Idle(conn));
    }
    err
}

/// Ends the read-only transaction `conn` holds, if one is owed, under DRAIN_CAP. A non-fatal
/// ROLLBACK error is kept as unreported; a fatal one, or the cap, closes the session.
async fn end_read_only(
    shared: &Shared,
    door: &Arc<KillDoor>,
    conn: Guarded,
) -> Result<Guarded, ExecuteError> {
    if !std::mem::take(&mut lock(shared).owed_rollback) {
        return Ok(conn);
    }
    let (conn, ended) = rollback(conn).await;
    rollback_ended(shared, door, conn, ended).await
}

/// What an owed ROLLBACK's result leaves: the connection at rest, with a non-fatal error
/// kept as unreported; a fatal error, or the cap, closes the session.
async fn rollback_ended(
    shared: &Shared,
    door: &Arc<KillDoor>,
    conn: Guarded,
    ended: Option<mysql_async::Result<u32>>,
) -> Result<Guarded, ExecuteError> {
    match ended {
        Some(Ok(_)) => Ok(conn),
        Some(Err(e)) if !e.is_fatal() => {
            lock(shared)
                .unreported
                .get_or_insert(execute_error(&e, None));
            Ok(conn)
        }
        Some(Err(_)) => Err(ExecuteError::SessionClosed),
        None => {
            kill_then_sever(door, Held::Idle(conn)).await;
            Err(ExecuteError::StoppedAtDrainCap)
        }
    }
}

/// A statement's own stop outranks the error that ending its read-only transaction met.
fn stop_or(finished: Result<(), ExecuteError>, ended: ExecuteError) -> ExecuteError {
    finished.err().unwrap_or(ended)
}

fn outcome(
    rows: u64,
    truncated_by: Option<TruncatedBy>,
    rows_affected: Option<u64>,
    extra_results: u32,
) -> Outcome {
    Outcome {
        rows,
        truncated: truncated_by.is_some(),
        rows_affected,
        truncated_by,
        extra_results,
        rolled_back: false,
    }
}

/// MySQL's binary collation; numbers, temporals and JSON report it too.
const BINARY_CHARSET: u16 = 63;

/// Whether every cell of `c` is bytes: BIT, GEOMETRY and VECTOR, and binary strings.
fn always_bytes(c: &Column) -> bool {
    use ColumnType::*;
    #[allow(
        clippy::wildcard_enum_match_arm,
        reason = "mysql ColumnType has too many variants to list; the rest share one arm"
    )]
    match c.column_type() {
        MYSQL_TYPE_BIT | MYSQL_TYPE_GEOMETRY | MYSQL_TYPE_VECTOR => true,
        MYSQL_TYPE_STRING
        | MYSQL_TYPE_VAR_STRING
        | MYSQL_TYPE_VARCHAR
        | MYSQL_TYPE_TINY_BLOB
        | MYSQL_TYPE_MEDIUM_BLOB
        | MYSQL_TYPE_LONG_BLOB
        | MYSQL_TYPE_BLOB => c.character_set() == BINARY_CHARSET,
        _ => false,
    }
}

/// The table column `c` reads, from its original names; none for a computed column.
fn source(c: &Column) -> Option<ColumnSource> {
    (!c.org_table_ref().is_empty()).then(|| ColumnSource::Mysql {
        schema: c.schema_str().into_owned(),
        table: c.org_table_str().into_owned(),
        column: c.org_name_str().into_owned(),
    })
}

/// The uppercase SQL name of the wire type, such as `BIGINT UNSIGNED`; ENUM and SET arrive as
/// flagged strings, and string types split into text and binary names by charset.
fn type_name(c: &Column) -> String {
    use ColumnType::*;
    let flags = c.flags();
    let binary = c.character_set() == BINARY_CHARSET;
    let pick = |text: &str, bin: &str| if binary { bin } else { text }.to_owned();
    let numeric = |name: &str| {
        let unsigned = flags.contains(ColumnFlags::UNSIGNED_FLAG);
        format!("{name}{}", if unsigned { " UNSIGNED" } else { "" })
    };
    match c.column_type() {
        MYSQL_TYPE_TINY => numeric("TINYINT"),
        MYSQL_TYPE_SHORT => numeric("SMALLINT"),
        MYSQL_TYPE_INT24 => numeric("MEDIUMINT"),
        MYSQL_TYPE_LONG => numeric("INT"),
        MYSQL_TYPE_LONGLONG => numeric("BIGINT"),
        MYSQL_TYPE_DECIMAL | MYSQL_TYPE_NEWDECIMAL => numeric("DECIMAL"),
        MYSQL_TYPE_FLOAT => numeric("FLOAT"),
        MYSQL_TYPE_DOUBLE => numeric("DOUBLE"),
        MYSQL_TYPE_YEAR => "YEAR".to_owned(),
        MYSQL_TYPE_DATE | MYSQL_TYPE_NEWDATE => "DATE".to_owned(),
        MYSQL_TYPE_DATETIME | MYSQL_TYPE_DATETIME2 => "DATETIME".to_owned(),
        MYSQL_TYPE_TIMESTAMP | MYSQL_TYPE_TIMESTAMP2 => "TIMESTAMP".to_owned(),
        MYSQL_TYPE_TIME | MYSQL_TYPE_TIME2 => "TIME".to_owned(),
        MYSQL_TYPE_BIT => "BIT".to_owned(),
        MYSQL_TYPE_JSON => "JSON".to_owned(),
        MYSQL_TYPE_GEOMETRY => "GEOMETRY".to_owned(),
        MYSQL_TYPE_ENUM => "ENUM".to_owned(),
        MYSQL_TYPE_SET => "SET".to_owned(),
        MYSQL_TYPE_STRING if flags.contains(ColumnFlags::ENUM_FLAG) => "ENUM".to_owned(),
        MYSQL_TYPE_STRING if flags.contains(ColumnFlags::SET_FLAG) => "SET".to_owned(),
        MYSQL_TYPE_STRING => pick("CHAR", "BINARY"),
        MYSQL_TYPE_VAR_STRING | MYSQL_TYPE_VARCHAR => pick("VARCHAR", "VARBINARY"),
        MYSQL_TYPE_TINY_BLOB | MYSQL_TYPE_MEDIUM_BLOB | MYSQL_TYPE_LONG_BLOB | MYSQL_TYPE_BLOB => {
            pick("TEXT", "BLOB")
        }
        other @ (MYSQL_TYPE_NULL
        | MYSQL_TYPE_TYPED_ARRAY
        | MYSQL_TYPE_VECTOR
        | MYSQL_TYPE_UNKNOWN) => {
            let wire = format!("{other:?}");
            wire.strip_prefix("MYSQL_TYPE_").unwrap_or(&wire).to_owned()
        }
    }
}

/// The session SETs sent once after the handshake: read-only, and MariaDB's own bound on
/// each statement. MySQL's max_execution_time bounds only SELECT, so the watchdog does it.
fn connect_sets(server: ServerInfo, read_only: bool, timeout: Option<Duration>) -> Option<String> {
    let mut sets = Vec::new();
    if read_only {
        sets.push("SET SESSION TRANSACTION READ ONLY".to_owned());
    }
    if let (Flavor::Mariadb, Some(t)) = (server.flavor, timeout) {
        sets.push(format!(
            "SET SESSION max_statement_time = {}",
            t.as_secs_f64()
        ));
    }
    (!sets.is_empty()).then(|| sets.join("; "))
}

/// The session variable that holds read-only: MariaDB before 11.1 knows only `tx_read_only`.
fn read_only_var(server: ServerInfo) -> &'static str {
    if server.flavor == Flavor::Mariadb && server.version < (11, 1, 0) {
        "tx_read_only"
    } else {
        "transaction_read_only"
    }
}

/// The round trip sent before each statement. Read-only re-sets the session first, since a
/// user SET may have lifted it, then opens the statement's read-only transaction.
fn statement_sets(
    server: ServerInfo,
    read_only: bool,
    timeout: Option<Duration>,
    raise_net_write: bool,
) -> Option<String> {
    let mut sets = Vec::new();
    if read_only {
        sets.push(format!("SESSION {} = 1", read_only_var(server)));
        if let (Flavor::Mariadb, Some(t)) = (server.flavor, timeout) {
            sets.push(format!("SESSION max_statement_time = {}", t.as_secs_f64()));
        }
    }
    if raise_net_write {
        sets.push("SESSION net_write_timeout = 3600".to_owned());
    }
    let set = (!sets.is_empty()).then(|| format!("SET {}", sets.join(", ")));
    if read_only {
        set.map(|s| format!("{s}; START TRANSACTION READ ONLY"))
    } else {
        set
    }
}

/// `tls = "verify"`: the built-in webpki roots plus the certificates in `tls_ca`, and the
/// client identity from `tls_cert` and `tls_key` when both are set. Errors are `Tls`.
fn ssl_opts(conn: &ConnectionConfig, passphrase: Option<&Secret>) -> Result<SslOpts, ConnectError> {
    let mut ssl = SslOpts::default();
    if let Some(path) = conn.tls_ca() {
        let fail = |e: String| tls_error(conn, format!("{}: {e}", path.display()));
        let bytes = std::fs::read(path).map_err(|e| fail(e.to_string()))?;
        let certs = CertificateDer::pem_slice_iter(&bytes)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| fail(e.to_string()))?;
        // A CA file that adds no roots is a mistake the user should hear about, as on Postgres.
        if certs.is_empty() {
            return Err(tls_error(
                conn,
                format!("no certificates in {}", path.display()),
            ));
        }
        ssl = ssl.with_root_certs(vec![bytes.into()]);
    }
    // client_auth answers Some only when both paths are set.
    if let Some((chain, key)) = client_auth(conn, passphrase)?
        && let (Some(cert_path), Some(key_path)) = (conn.tls_cert(), conn.tls_key())
    {
        let fail =
            |path: &std::path::Path, e: String| tls_error(conn, format!("{}: {e}", path.display()));
        let mut chain_pem = String::new();
        for cert in &chain {
            chain_pem.push_str(
                &pem::encode_string("CERTIFICATE", LineEnding::LF, cert.as_ref())
                    .map_err(|e| fail(cert_path, e.to_string()))?,
            );
        }
        // PEM, never DER: mysql_async reads a non-UTF-8 key buffer as PKCS#1.
        let key = key_pem(&key).map_err(|e| fail(key_path, e))?;
        let identity = ClientIdentity::new(chain_pem.into_bytes().into(), key.into_bytes().into());
        ssl = ssl.with_client_identity(Some(identity));
    }
    Ok(ssl)
}

/// The key as PEM under its own label; a key kind rustls adds later is an error naming it.
pub(crate) fn key_pem(key: &PrivateKeyDer<'_>) -> Result<String, String> {
    let label = match key {
        PrivateKeyDer::Pkcs8(_) => "PRIVATE KEY",
        PrivateKeyDer::Pkcs1(_) => "RSA PRIVATE KEY",
        PrivateKeyDer::Sec1(_) => "EC PRIVATE KEY",
        other => return Err(format!("unsupported key kind {other:?}")),
    };
    pem::encode_string(label, LineEnding::LF, key.secret_der()).map_err(|e| e.to_string())
}

/// A certificate or TLS failure under `tls = "verify"`, naming both ways out; `None` for
/// any other error. tokio-rustls hands a handshake failure back as an io::Error around it.
fn tls_failure(conn: &ConnectionConfig, e: &mysql_async::Error) -> Option<ConnectError> {
    let cause = match e {
        mysql_async::Error::Io(IoError::Tls(t)) => t.to_string(),
        mysql_async::Error::Io(IoError::Io(io)) => io
            .get_ref()
            .and_then(|inner| inner.downcast_ref::<rustls::Error>())?
            .to_string(),
        mysql_async::Error::Driver(DriverError::NoClientSslFlagFromServer) => e.to_string(),
        mysql_async::Error::Driver(_)
        | mysql_async::Error::Other(_)
        | mysql_async::Error::Server(_)
        | mysql_async::Error::Url(_) => return None,
    };
    Some(tls_error(
        conn,
        format!("{cause}; {TLS_WAYS_OUT} to connect without TLS"),
    ))
}

/// A server error: `<display_target>: <message> (<number>)`; a TLS failure under verify is
/// `Tls`; anything else names the target.
fn connect_error(conn: &ConnectionConfig, e: &mysql_async::Error) -> ConnectError {
    if conn.tls() == Some(TlsMode::Verify)
        && let Some(tls) = tls_failure(conn, e)
    {
        return tls;
    }
    let target = conn.display_target();
    let message = match e {
        mysql_async::Error::Server(s) => format!("{target}: {} ({})", s.message, s.code),
        other @ (mysql_async::Error::Driver(_)
        | mysql_async::Error::Io(_)
        | mysql_async::Error::Other(_)
        | mysql_async::Error::Url(_)) => format!("{target}: {other}"),
    };
    ConnectError::Open { message }
}

/// Server errors that mean the statement was interrupted: a KILL (1317), a sort the kill
/// aborted (1028), or MariaDB's max_statement_time (1969).
fn interrupted(e: &mysql_async::Error) -> bool {
    matches!(e, mysql_async::Error::Server(s) if matches!(s.code, 1317 | 1028 | 1969))
}

/// Server error: Database { code: number, sqlstate: Some(state), message }, except that an
/// interrupted statement Dbento stopped reports why, and MariaDB's own timeout is `Timeout`;
/// anything else is SessionClosed.
fn execute_error(e: &mysql_async::Error, stop: Option<Stop>) -> ExecuteError {
    if let Some(stop) = stop.filter(|_| interrupted(e)) {
        return stop.error();
    }
    match e {
        mysql_async::Error::Server(s) if s.code == 1969 => ExecuteError::Timeout,
        mysql_async::Error::Server(s) if s.code == 1792 => ExecuteError::ReadOnlyViolation {
            message: s.message.clone(),
        },
        mysql_async::Error::Server(s) => ExecuteError::Database {
            code: s.code.to_string(),
            sqlstate: Some(s.state.clone()),
            class: None,
            state: None,
            message: s.message.clone(),
            detail: None,
            hint: None,
            position: None,
        },
        mysql_async::Error::Driver(_)
        | mysql_async::Error::Io(_)
        | mysql_async::Error::Other(_)
        | mysql_async::Error::Url(_) => ExecuteError::SessionClosed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn server_error(code: u16) -> mysql_async::Error {
        mysql_async::Error::Server(mysql_async::ServerError {
            code,
            message: "interrupted".to_owned(),
            state: "HY000".to_owned(),
        })
    }

    fn server(flavor: Flavor, version: (u16, u16, u16)) -> ServerInfo {
        ServerInfo { flavor, version }
    }

    #[test]
    fn a_tls_failure_under_verify_ends_with_the_shared_ways_out() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("dbento.toml"), "dbento = 1\n[connections.m]\nengine = \"mysql\"\nhost = \"h\"\nuser = \"u\"\ntls = \"verify\"\nsecret = \"none\"\n").unwrap();
        let ws = crate::Workspace::open(dir.path()).unwrap();
        let e = mysql_async::Error::Driver(DriverError::NoClientSslFlagFromServer);
        match tls_failure(ws.connection("m").unwrap(), &e) {
            Some(ConnectError::Tls { message, .. }) => assert_eq!(
                message,
                format!("{e}; {TLS_WAYS_OUT} to connect without TLS")
            ),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn client_keys_become_pem_with_their_own_label() {
        use rustls::pki_types::{PrivatePkcs1KeyDer, PrivatePkcs8KeyDer, PrivateSec1KeyDer};
        let der = vec![1u8, 2, 3];
        for (key, label) in [
            (
                PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(der.clone())),
                "PRIVATE KEY",
            ),
            (
                PrivateKeyDer::Pkcs1(PrivatePkcs1KeyDer::from(der.clone())),
                "RSA PRIVATE KEY",
            ),
            (
                PrivateKeyDer::Sec1(PrivateSec1KeyDer::from(der.clone())),
                "EC PRIVATE KEY",
            ),
        ] {
            let pem = key_pem(&key).unwrap();
            assert!(
                pem.starts_with(&format!("-----BEGIN {label}-----")),
                "{pem}"
            );
        }
    }

    #[test]
    fn connect_sets_read_only_and_mariadbs_own_timeout() {
        let (my, maria) = (
            server(Flavor::Mysql, (8, 4, 13)),
            server(Flavor::Mariadb, (11, 4, 13)),
        );
        let t = Some(Duration::from_millis(1500));
        assert_eq!(
            connect_sets(my, true, t).as_deref(),
            Some("SET SESSION TRANSACTION READ ONLY")
        );
        assert_eq!(
            connect_sets(maria, true, t).as_deref(),
            Some("SET SESSION TRANSACTION READ ONLY; SET SESSION max_statement_time = 1.5")
        );
        assert_eq!(
            connect_sets(maria, false, Some(Duration::from_secs(2))).as_deref(),
            Some("SET SESSION max_statement_time = 2")
        );
        assert_eq!(connect_sets(my, false, t), None);
        assert_eq!(connect_sets(maria, false, None), None);
    }

    #[test]
    fn mariadbs_statement_timeout_is_a_timeout_unless_a_cancel_was_sent() {
        let e = execute_error(&server_error(1969), None);
        assert!(matches!(e, ExecuteError::Timeout), "{e:?}");
        let e = execute_error(&server_error(1969), Some(Stop::Cancelled));
        assert!(matches!(e, ExecuteError::Cancelled), "{e:?}");
        let e = execute_error(&server_error(1317), Some(Stop::Timeout));
        assert!(matches!(e, ExecuteError::Timeout), "{e:?}");
    }

    #[test]
    fn a_reported_stop_hides_only_the_interruption_it_caused() {
        for code in [1317, 1028, 1969] {
            assert!(drain_report(&server_error(code), true, Some(Stop::Cancelled)).is_none());
        }
        let e = drain_report(&server_error(1213), true, Some(Stop::Cancelled));
        assert!(
            matches!(e, Some(ExecuteError::Database { ref code, .. }) if code == "1213"),
            "{e:?}"
        );
        let e = drain_report(&server_error(1317), false, Some(Stop::Cancelled));
        assert!(matches!(e, Some(ExecuteError::Cancelled)), "{e:?}");
    }

    #[test]
    fn a_stop_outranks_the_error_its_rollback_met() {
        let e = stop_or(Err(ExecuteError::Timeout), ExecuteError::SessionClosed);
        assert!(matches!(e, ExecuteError::Timeout), "{e:?}");
        let e = stop_or(
            Err(ExecuteError::Cancelled),
            ExecuteError::StoppedAtDrainCap,
        );
        assert!(matches!(e, ExecuteError::Cancelled), "{e:?}");
        let e = stop_or(Ok(()), ExecuteError::StoppedAtDrainCap);
        assert!(matches!(e, ExecuteError::StoppedAtDrainCap), "{e:?}");
    }

    #[test]
    fn a_sort_aborted_by_a_stop_reports_the_stop() {
        let e = execute_error(&server_error(1028), Some(Stop::Cancelled));
        assert!(matches!(e, ExecuteError::Cancelled), "{e:?}");
        let e = execute_error(&server_error(1028), None);
        assert!(
            matches!(e, ExecuteError::Database { ref code, .. } if code == "1028"),
            "{e:?}"
        );
    }

    #[test]
    fn the_read_only_variable_follows_flavor_and_version() {
        assert_eq!(
            read_only_var(server(Flavor::Mysql, (8, 4, 13))),
            "transaction_read_only"
        );
        assert_eq!(
            read_only_var(server(Flavor::Mariadb, (11, 1, 0))),
            "transaction_read_only"
        );
        assert_eq!(
            read_only_var(server(Flavor::Mariadb, (10, 11, 19))),
            "tx_read_only"
        );
    }

    #[test]
    fn a_read_only_statement_resets_the_session_then_opens_its_transaction() {
        let my = server(Flavor::Mysql, (8, 4, 13));
        let maria = server(Flavor::Mariadb, (11, 4, 13));
        let t = Some(Duration::from_secs(2));
        assert_eq!(
            statement_sets(my, true, t, false).as_deref(),
            Some("SET SESSION transaction_read_only = 1; START TRANSACTION READ ONLY")
        );
        assert_eq!(
            statement_sets(maria, true, t, true).as_deref(),
            Some(
                "SET SESSION transaction_read_only = 1, SESSION max_statement_time = 2, \
                 SESSION net_write_timeout = 3600; START TRANSACTION READ ONLY"
            )
        );
        assert_eq!(
            statement_sets(server(Flavor::Mariadb, (10, 11, 19)), true, None, false).as_deref(),
            Some("SET SESSION tx_read_only = 1; START TRANSACTION READ ONLY")
        );
        assert_eq!(
            statement_sets(my, false, t, true).as_deref(),
            Some("SET SESSION net_write_timeout = 3600")
        );
        assert_eq!(statement_sets(maria, false, t, false), None);
    }

    #[test]
    fn only_a_parsed_write_or_an_unpreparable_single_statement_skips_the_prepare_check() {
        assert!(!prepare_refused(1295, "USE shop"));
        assert!(!prepare_refused(1792, "DELETE FROM t"));
        assert!(prepare_refused(1295, "USE shop; DELETE FROM t"));
        assert!(prepare_refused(1064, "SELECT 1; DELETE FROM t"));
        assert!(prepare_refused(1146, "SELECT * FROM missing"));
    }

    // While the test holds `inflight`, a stop must not yet have handed its KILL to the killer:
    // otherwise `spend()` could run in that gap, miss the KILL, and let it land on g+1.
    #[test]
    fn a_stop_is_in_flight_before_its_kill_is_sent() {
        let door = Arc::new(KillDoor {
            opts: Opts::from(OptsBuilder::default()),
            thread: 1,
            target: "test".to_owned(),
        });
        let cancel = Arc::new(MysqlCancel::new(door));
        let (jobs, mut rx) = tokio::sync::mpsc::unbounded_channel::<KillJob>();
        assert!(cancel.killer.set(Ok(jobs)).is_ok());
        lock(&cancel.target).running = Some(1);

        let held = lock(&cancel.inflight);
        let stopper = {
            let cancel = Arc::clone(&cancel);
            std::thread::spawn(move || cancel.stop(1, Stop::Cancelled))
        };
        std::thread::sleep(Duration::from_millis(300));
        assert!(
            rx.try_recv().is_err(),
            "the KILL was sent before the stop was in flight"
        );
        drop(held);
        stopper.join().unwrap();
        assert_eq!(rx.try_recv().map(|(g, _)| g).ok(), Some(1));
        assert_eq!(lock(&cancel.inflight).len(), 1);
    }
}
