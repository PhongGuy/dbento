//! The Redis session: a `Driver` on tokio over one multiplexed connection, which the session drops
//! the moment the transport fails, ending it. Every statement is one raw command whose reply,
//! errors included, comes back as a value.

use std::collections::{BTreeSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ::redis::aio::MultiplexedConnection;
use ::redis::{
    AsyncConnectionConfig, Client, ClientTlsConfig, Cmd, ConnectionAddr, ErrorKind,
    IntoConnectionInfo, Pipeline, ProtocolVersion, RedisConnectionInfo, RedisError,
    TlsCertificates, Value,
};
use pkcs8::der::pem::{self, LineEnding};
use rustls::pki_types::CertificateDer;
use rustls::pki_types::pem::PemObject as _;
use tokio::sync::watch;

use super::browse::{KeyBrowseError, server_error};
use super::classify::redis_classification;
use super::lex::{RedisStatement, SyntaxError, parse_script};
use super::probe::{gate, probe};
use super::shape::{ShapeError, reply_to_rows};
use super::stream::RedisStream;
use super::table::CommandTable;
use crate::Secret;
use crate::introspect::{ColumnMeta, IntrospectError, RelationList, RelationRef, SchemaInfo};
use crate::mysql::key_pem;
use crate::postgres::{TLS_WAYS_OUT, client_auth, tls_error};
use crate::session::{
    CancelHandle, Classification, ConnectError, ConnectParams, Driver, ExecuteError, ExtendError,
    Flavor, Limits, OnTruncate, Prepared, ReadOnlyEnforcement, ServerInfo, StreamItem, Writes,
};
use crate::value::ColumnInfo;
use crate::workspace::{ConnectionConfig, TlsMode};

pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// What the catalog calls that read tables answer: Redis has none.
pub(crate) const NO_TABLES: &str = "tables (Redis has keys: browse them with SCAN)";

/// The oldest server whose `COMMAND` reply carries key specs and docs.
const OLDEST: (u16, u16, u16) = (7, 0, 0);

/// What the session, its script and its stream all see.
pub(crate) struct Shared {
    /// The connection; emptied when the session ends, which drops the socket with the last clone.
    conn: Mutex<Option<MultiplexedConnection>>,
    pub(super) table: CommandTable,
    pub(super) enforcement: Option<ReadOnlyEnforcement>,
    /// Entry names the read-only probe could not reach; see `Probe::unprobeable`.
    pub(super) unprobeable: BTreeSet<String>,
    /// Why the session ended, once it has.
    ended: watch::Sender<Option<String>>,
    pub(super) settings_changed: AtomicBool,
    /// The statement timeout; each round trip is bounded by it.
    timeout: Option<Duration>,
    cancel: Arc<RedisCancel>,
    state: Mutex<State>,
}

/// What the server's connection state is, as far as the session has seen it.
#[derive(Debug)]
struct State {
    /// The selected DB index; `None` once a transaction may have moved it and the server
    /// would not say where, until a tracked `SELECT` names it again.
    db: Option<u32>,
    /// A `MULTI` was answered `OK` and no `EXEC` or `DISCARD` has answered since.
    in_multi: bool,
}

/// Why a call that selects other DBs and comes back cannot run now.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Unbracketable {
    InMulti,
    DbUnknown,
}

impl Unbracketable {
    pub(crate) fn reason(self) -> &'static str {
        match self {
            Unbracketable::InMulti => IN_MULTI,
            Unbracketable::DbUnknown => {
                "the session's DB is unknown after a transaction; send SELECT n first"
            }
        }
    }
}

/// Why the key browser and the catalog refuse inside a transaction.
pub(crate) const IN_MULTI: &str = "the session is inside MULTI; send EXEC or DISCARD first";

/// Why a round trip did not answer.
#[derive(Debug)]
pub(crate) enum Failure {
    /// The session ended, now or earlier, for this reason.
    Ended(String),
    /// The statement timeout passed; the session has ended.
    Timeout,
    /// The statement was cancelled; the session has ended.
    Cancelled,
}

impl From<Failure> for ExecuteError {
    fn from(failure: Failure) -> Self {
        match failure {
            Failure::Ended(reason) => ExecuteError::SessionEnded {
                reason: format!("{reason}; reopen the session"),
            },
            Failure::Timeout => ExecuteError::Timeout,
            Failure::Cancelled => ExecuteError::Cancelled,
        }
    }
}

impl From<Failure> for IntrospectError {
    fn from(failure: Failure) -> Self {
        IntrospectError::Execute(failure.into())
    }
}

impl Shared {
    /// Sends `cmd` and returns its reply; a server error is a reply, a transport error ends
    /// the session.
    pub(super) async fn one(&self, cmd: &Cmd) -> Result<Value, Failure> {
        let mut conn = self.connection()?;
        self.guarded(async move { conn.send_packed_command(cmd).await })
            .await
    }

    /// Sends `pipe` and returns one reply per command, under the same guard as `one`.
    pub(super) async fn many(&self, pipe: &Pipeline) -> Result<Vec<Value>, Failure> {
        let mut conn = self.connection()?;
        self.guarded(async move { conn.send_packed_commands(pipe, 0, pipe.len()).await })
            .await
    }

    /// Runs `pipe` on DB `db` and returns its replies: `SELECT db`, the pipe and a `SELECT` back
    /// to the session's DB go as one pipeline, so the editor's DB is where it was after.
    pub(super) async fn bracketed(
        &self,
        db: u32,
        pipe: Pipeline,
    ) -> Result<Vec<Value>, KeyBrowseError> {
        let back = self
            .bracket_db()
            .map_err(|refusal| KeyBrowseError::Unsupported {
                reason: refusal.reason().to_owned(),
            })?;
        let mut whole = Pipeline::with_capacity(pipe.len() + 2);
        whole.cmd("SELECT").arg(db);
        for cmd in pipe.cmd_iter() {
            whole.add_command(cmd.clone());
        }
        whole.cmd("SELECT").arg(back);
        let mut replies = self.many(&whole).await?;
        if replies.len() != whole.len() {
            return Err(KeyBrowseError::Database {
                code: "ERR".to_owned(),
                message: format!("{} replies to {} commands", replies.len(), whole.len()),
            });
        }
        let restored = replies.pop().map(server_error);
        if !matches!(restored, Some(Ok(_))) {
            self.forget_db();
        }
        server_error(replies.remove(0))?;
        if let Some(Err(e)) = restored {
            return Err(e);
        }
        Ok(replies)
    }

    /// `f` under the statement timeout and the cancel; a timeout, a cancel or a transport error
    /// ends the session. The connection clone inside `f` drops with `f`, so emptying the slot
    /// drops the socket and the server stops waiting on the command.
    async fn guarded<T>(
        &self,
        f: impl Future<Output = ::redis::RedisResult<T>>,
    ) -> Result<T, Failure> {
        let bounded = async {
            match self.timeout {
                Some(limit) => tokio::time::timeout(limit, f)
                    .await
                    .map_err(|_| Failure::Timeout),
                None => Ok(f.await),
            }
        };
        match self.cancel.guard(bounded).await {
            Err(_) => {
                self.end("the statement was cancelled".to_owned());
                Err(Failure::Cancelled)
            }
            Ok(Err(timeout)) => {
                self.end("the statement timed out".to_owned());
                Err(timeout)
            }
            Ok(Ok(Err(e))) => {
                let reason = format!("the connection failed: {e}");
                self.end(reason.clone());
                Err(Failure::Ended(reason))
            }
            Ok(Ok(Ok(v))) => Ok(v),
        }
    }

    /// Follows `SELECT`, `MULTI`, `EXEC` and `DISCARD` from their replies. A `SELECT` queued in
    /// a transaction answers `QUEUED` and moves the DB only at `EXEC`, so after an `EXEC` that
    /// ended a `MULTI` the server is asked where it is.
    async fn track(&self, args: &[Vec<u8>], reply: &Value) -> Result<(), Failure> {
        let Some(name) = args.first() else {
            return Ok(());
        };
        let ok =
            matches!(reply, Value::Okay) || matches!(reply, Value::SimpleString(s) if s == "OK");
        let name = name.to_ascii_uppercase();
        match name.as_slice() {
            b"SELECT" if ok => {
                let index = args
                    .get(1)
                    .and_then(|a| std::str::from_utf8(a).ok())
                    .and_then(|a| a.parse::<u32>().ok());
                if let Some(index) = index {
                    self.state().db = Some(index);
                    self.settings_changed.store(true, Ordering::Relaxed);
                }
            }
            b"MULTI" if ok => self.state().in_multi = true,
            b"DISCARD" => self.state().in_multi = false,
            b"EXEC" => {
                let ended_multi = std::mem::replace(&mut self.state().in_multi, false);
                if !ended_multi {
                    return Ok(());
                }
                let info = self.one(::redis::cmd("CLIENT").arg("INFO")).await?;
                let db = text_of(&info).and_then(|line| {
                    line.split_whitespace()
                        .find_map(|f| f.strip_prefix("db="))
                        .and_then(|d| d.parse::<u32>().ok())
                });
                let mut state = self.state();
                if db != state.db {
                    state.db = db;
                    self.settings_changed.store(true, Ordering::Relaxed);
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// The session's DB, for a call that selects others and comes back. Refused inside `MULTI`,
    /// where a `SELECT` would only be queued, and while the session's DB is unknown.
    pub(super) fn bracket_db(&self) -> Result<u32, Unbracketable> {
        let state = self.state();
        if state.in_multi {
            return Err(Unbracketable::InMulti);
        }
        state.db.ok_or(Unbracketable::DbUnknown)
    }

    pub(super) fn in_multi(&self) -> bool {
        self.state().in_multi
    }

    /// Marks the session's DB unknown after a restoring `SELECT` that did not answer `OK`.
    pub(super) fn forget_db(&self) {
        self.state().db = None;
        self.settings_changed.store(true, Ordering::Relaxed);
    }

    fn state(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Empties the slot and publishes `reason`; the first reason stands.
    pub(super) fn end(&self, reason: String) {
        *self.conn.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.ended.send_if_modified(|ended| {
            let first = ended.is_none();
            if first {
                *ended = Some(reason);
            }
            first
        });
    }

    fn connection(&self) -> Result<MultiplexedConnection, Failure> {
        let slot = self.conn.lock().unwrap_or_else(|e| e.into_inner());
        slot.clone().ok_or_else(|| {
            let reason = self.ended.borrow().clone();
            Failure::Ended(reason.unwrap_or_else(|| "the connection closed".to_owned()))
        })
    }
}

/// Cancels by generation: `cancel` bumps it, `settle` spends it.
#[derive(Debug)]
pub(crate) struct RedisCancel {
    generation: watch::Sender<u64>,
    spent: AtomicU64,
}

impl RedisCancel {
    pub(crate) fn new() -> Self {
        RedisCancel {
            generation: watch::Sender::new(0),
            spent: AtomicU64::new(0),
        }
    }

    pub(crate) fn cancel(&self) {
        // `send_modify` counts the cancel even with no statement listening.
        self.generation.send_modify(|g| *g += 1);
    }

    pub(crate) fn spend(&self) {
        let generation = *self.generation.borrow();
        self.spent.store(generation, Ordering::Relaxed);
    }

    /// `Err(Cancelled)` at once for an unspent cancel, or when one arrives while `f` runs, which
    /// drops `f`.
    pub(crate) async fn guard<F: Future>(&self, f: F) -> Result<F::Output, ExecuteError> {
        let mut generation = self.generation.subscribe();
        // Discards `wait_for`'s reply here: it holds the channel's lock, which `spend` needs.
        let cancelled = async {
            let unspent = |g: &u64| *g != self.spent.load(Ordering::Relaxed);
            let _ = generation.wait_for(unspent).await;
        };
        // Biased so an unspent cancel wins before `f` is first polled, and nothing is sent.
        tokio::select! {
            biased;
            () = cancelled => {
                self.spend();
                Err(ExecuteError::Cancelled)
            }
            out = f => Ok(out),
        }
    }
}

pub(crate) struct RedisSession {
    pub(super) shared: Arc<Shared>,
    server: ServerInfo,
    cancel: Arc<RedisCancel>,
}

/// The parsed lines of one script; `current` is the one prepared and not yet run.
pub(crate) struct RedisScript {
    shared: Arc<Shared>,
    src: String,
    statements: VecDeque<Result<RedisStatement, SyntaxError>>,
    current: Option<(RedisStatement, Classification)>,
}

impl RedisSession {
    pub(crate) fn server(&self) -> ServerInfo {
        self.server
    }

    pub(crate) fn settings_changed(&self) -> bool {
        self.shared.settings_changed.load(Ordering::Relaxed)
    }

    /// Resolves once the session has ended.
    pub(crate) async fn closed(&self) {
        let mut ended = self.shared.ended.subscribe();
        let _ = ended.wait_for(Option::is_some).await;
    }
}

impl Driver for RedisSession {
    type Script = RedisScript;
    type Stream = RedisStream;

    async fn connect(params: ConnectParams<'_>) -> Result<Self, ConnectError> {
        let conn = params.conn;
        let target = conn.display_target();
        let client = client(conn, params.secret, params.key_passphrase)?;
        // The driver bounds each round trip itself: a multiplexed connection's response
        // timeout can be set later but never cleared.
        let config = AsyncConnectionConfig::new()
            .set_connection_timeout(Some(CONNECT_TIMEOUT))
            .set_response_timeout(None);
        let mut connection = within(
            &target,
            client.get_multiplexed_async_connection_with_config(&config),
        )
        .await?
        .map_err(|e| dial_error(conn, &target, e))?;

        // redis-rs discards its own HELLO's reply, so the server is asked again.
        let hello = handshake(
            &target,
            &mut connection,
            ::redis::cmd("HELLO").arg(3),
            "hello",
        )
        .await?;
        let server = server_info(&hello).map_err(|reason| ConnectError::Unsupported {
            reason: format!("{target}: {reason}"),
        })?;
        let reply = handshake(
            &target,
            &mut connection,
            &::redis::cmd("COMMAND"),
            "command",
        )
        .await?;
        let table = CommandTable::from_value(&reply).map_err(|e| ConnectError::Open {
            message: format!("{target}: {e}"),
        })?;
        // Every read-only connect asks the server's ACL afresh; a writable session never probes.
        let (enforcement, unprobeable) = if params.read_only {
            let found = within(&target, probe(&mut connection, &table))
                .await?
                .map_err(|e| match e {
                    ConnectError::Open { message } => ConnectError::Open {
                        message: format!("{target}: {message}"),
                    },
                    other @ (ConnectError::NotFound { .. }
                    | ConnectError::Tls { .. }
                    | ConnectError::InvalidTimeout { .. }
                    | ConnectError::Refused { .. }
                    | ConnectError::Unsupported { .. }) => other,
                })?;
            (Some(found.level), found.unprobeable)
        } else {
            (None, BTreeSet::new())
        };
        let cancel = Arc::new(RedisCancel::new());
        Ok(RedisSession {
            shared: Arc::new(Shared {
                conn: Mutex::new(Some(connection)),
                table,
                enforcement,
                unprobeable,
                ended: watch::Sender::new(None),
                settings_changed: AtomicBool::new(false),
                timeout: params.timeout,
                cancel: Arc::clone(&cancel),
                state: Mutex::new(State {
                    db: Some(db_index(conn)?),
                    in_multi: false,
                }),
            }),
            server,
            cancel,
        })
    }

    fn start_script(&mut self, text: &str) -> RedisScript {
        RedisScript {
            shared: Arc::clone(&self.shared),
            src: text.to_owned(),
            statements: parse_script(text).into(),
            current: None,
        }
    }

    async fn prepare_next(script: &mut RedisScript) -> Option<Result<Prepared, ExecuteError>> {
        let next = script.statements.pop_front()?;
        let prepared = next
            .map_err(|e| ExecuteError::Syntax {
                message: e.message,
                offset: e.offset,
            })
            .and_then(|statement| {
                let classification = redis_classification(&script.shared.table, &statement)
                    .map_err(|refusal| ExecuteError::Unsupported {
                        reason: refusal.reason,
                    })?;
                Ok((statement, classification))
            });
        let (statement, classification) = match prepared {
            Ok(prepared) => prepared,
            Err(failure) => {
                // A refused or unsplittable line ends its script, so no later line runs.
                script.statements.clear();
                return Some(Err(failure));
            }
        };
        let prepared = Prepared {
            sql: script.src[statement.range.clone()].to_owned(),
            classification: classification.clone(),
        };
        script.current = Some((statement, classification));
        Some(Ok(prepared))
    }

    async fn is_last(script: &mut RedisScript) -> bool {
        script.statements.is_empty()
    }

    async fn execute(
        script: &mut RedisScript,
        limits: Limits,
    ) -> Result<(Vec<ColumnInfo>, RedisStream), ExecuteError> {
        let (statement, classification) =
            script.current.take().ok_or(ExecuteError::SessionClosed)?;
        let shared = &script.shared;
        let entry = shared.table.entry(&statement.args);
        let unprobeable = entry.is_some_and(|e| shared.unprobeable.contains(&e.name));
        if let Some(refusal) = gate(&classification, unprobeable, shared.enforcement) {
            return Err(refusal);
        }
        let mut cmd = Cmd::new();
        for word in &statement.args {
            cmd.arg(word.as_slice());
        }
        let reply = shared.one(&cmd).await?;
        shared.track(&statement.args, &reply).await?;
        // The ACL fencing a command that may write is the read-only session working as meant.
        if let Value::ServerError(e) = &reply
            && e.code() == "NOPERM"
            && shared.enforcement.is_some()
            && matches!(classification.writes, Writes::Yes | Writes::Unknown)
        {
            return Err(ExecuteError::ReadOnlyViolation {
                message: e.details().unwrap_or(e.code()).to_owned(),
            });
        }
        // A write's integer is also its count, so `DEL` reads as rows affected beside its row.
        let writes = entry.is_some_and(|e| e.has_flag("write"));
        #[allow(
            clippy::wildcard_enum_match_arm,
            reason = "Value has too many variants to list; the rest share one arm"
        )]
        let affected = match reply {
            Value::Int(n) if writes => u64::try_from(n).ok(),
            _ => None,
        };
        // A parked reply keeps every row for `extend`; otherwise rows past the cap are not shaped.
        let cap = match limits.on_truncate {
            OnTruncate::Park => None,
            OnTruncate::Drain | OnTruncate::Close => limits.max_rows,
        };
        let shaped = reply_to_rows(reply, cap).map_err(shape_failure)?;
        let (columns, stream) = RedisStream::new(shaped, limits, affected);
        if limits.on_truncate == OnTruncate::Close && stream.truncated() {
            shared.end("the result was cut short and on_truncate is Close".to_owned());
        }
        Ok((columns, stream))
    }

    async fn next_event(stream: &mut RedisStream) -> Result<StreamItem, ExecuteError> {
        Ok(stream.next_event())
    }

    fn extend(stream: &mut RedisStream, more: Option<u64>) -> Result<(), ExtendError> {
        stream.extend(more)
    }

    fn cancel_handle(&self) -> CancelHandle {
        CancelHandle::redis(Arc::clone(&self.cancel))
    }

    async fn settle(&mut self) -> Result<(), ExecuteError> {
        self.cancel.spend();
        self.shared
            .connection()
            .map(drop)
            .map_err(ExecuteError::from)
    }

    async fn close(self) -> Result<(), ExecuteError> {
        self.shared.end("the session was closed".to_owned());
        Ok(())
    }

    async fn schemas(&mut self) -> Result<Vec<SchemaInfo>, IntrospectError> {
        self.shared.schemas().await
    }

    async fn relations(&mut self, _schema: &str) -> Result<RelationList, IntrospectError> {
        Err(IntrospectError::Unsupported { what: NO_TABLES })
    }

    async fn columns(&mut self, _rel: &RelationRef) -> Result<Vec<ColumnMeta>, IntrospectError> {
        Err(IntrospectError::Unsupported { what: NO_TABLES })
    }

    fn read_only_level(&self, _requested: bool) -> Option<ReadOnlyEnforcement> {
        self.shared.enforcement
    }
}

/// A reply that is not rows, as the statement's failure.
fn shape_failure(e: ShapeError) -> ExecuteError {
    match e {
        ShapeError::Server { code, message } => {
            let redirected = matches!(code.as_str(), "MOVED" | "ASK");
            // A bare code has no message, and the code alone then says what failed.
            let message = if message.is_empty() {
                code.clone()
            } else {
                message
            };
            let mut failure = ExecuteError::database(code, message);
            if redirected && let ExecuteError::Database { hint, .. } = &mut failure {
                *hint = Some("cluster mode is not supported".to_owned());
            }
            failure
        }
        other @ (ShapeError::Push | ShapeError::Unknown) => ExecuteError::Unsupported {
            reason: other.to_string(),
        },
    }
}

/// `ServerInfo` from a `HELLO 3` reply: `server` names the flavor, `version` must be 7.0 or later.
pub(crate) fn server_info(hello: &Value) -> Result<ServerInfo, String> {
    #[allow(
        clippy::wildcard_enum_match_arm,
        reason = "Value has too many variants to list; the rest share one arm"
    )]
    let field = |name: &str| match hello {
        Value::Map(pairs) => pairs
            .iter()
            .find(|(k, _)| text_of(k).as_deref() == Some(name))
            .and_then(|(_, v)| text_of(v)),
        _ => None,
    };
    let flavor = match field("server").as_deref() {
        Some("valkey") => Flavor::Valkey,
        _ => Flavor::Redis,
    };
    let text = field("version").ok_or("the server's HELLO reply names no version")?;
    let version = version_of(&text).ok_or_else(|| format!("unexpected server version {text:?}"))?;
    if version < OLDEST {
        return Err(too_old(&format!("version {text}")));
    }
    Ok(ServerInfo { flavor, version })
}

fn too_old(what: &str) -> String {
    format!("the server runs {what}, and Dbento needs Redis 7.0 or later")
}

pub(super) fn text_of(value: &Value) -> Option<String> {
    #[allow(
        clippy::wildcard_enum_match_arm,
        reason = "Value has too many variants to list; the rest share one arm"
    )]
    match value {
        Value::BulkString(bytes) => String::from_utf8(bytes.clone()).ok(),
        Value::SimpleString(text) => Some(text.clone()),
        Value::VerbatimString { text, .. } => Some(text.clone()),
        _ => None,
    }
}

/// Major, minor and patch of a version such as "9.0.7" or "7.2.0-rc1".
fn version_of(text: &str) -> Option<(u16, u16, u16)> {
    let mut parts = text.split(['.', '-']).map(|p| p.parse::<u16>().ok());
    Some((parts.next()??, parts.next()??, parts.next()??))
}

/// The client for `conn`: RESP3, so redis-rs's own `HELLO 3` carries the login, and the DB index.
pub(crate) fn client(
    conn: &ConnectionConfig,
    secret: Option<&Secret>,
    passphrase: Option<&Secret>,
) -> Result<Client, ConnectError> {
    let host = conn.host().unwrap_or("127.0.0.1").to_owned();
    let port = conn.port().unwrap_or(6379);
    let addr = match conn.tls() {
        Some(TlsMode::Verify) => ConnectionAddr::TcpTls {
            host,
            port,
            insecure: false,
            tls_params: None,
        },
        _ => ConnectionAddr::Tcp(host, port),
    };
    let invalid = |e: RedisError| ConnectError::Open {
        message: format!("{}: {e}", conn.display_target()),
    };
    let mut settings = RedisConnectionInfo::default()
        .set_protocol(ProtocolVersion::RESP3)
        .set_db(i64::from(db_index(conn)?))
        .set_lib_name("dbento", env!("CARGO_PKG_VERSION"));
    if let Some(user) = conn.user() {
        settings = settings.set_username(user);
    }
    if let Some(secret) = secret {
        settings = settings.set_password(secret.expose());
    }
    let info = addr
        .into_connection_info()
        .map_err(invalid)?
        .set_redis_settings(settings);
    match tls_certificates(conn, passphrase)? {
        Some(certs) => {
            Client::build_with_tls(info, certs).map_err(|e| tls_error(conn, e.to_string()))
        }
        None => Client::open(info).map_err(invalid),
    }
}

/// The connection's DB index, 0 when it names none.
fn db_index(conn: &ConnectionConfig) -> Result<u32, ConnectError> {
    let db = conn.database().unwrap_or("0");
    db.parse::<u32>().map_err(|_| ConnectError::Open {
        message: format!("{}: `{db}` is not a DB index", conn.display_target()),
    })
}

/// The CA and client pair under `tls = "verify"`, PEM-encoded as redis-rs takes them; `None`
/// when neither is set. A `tls_ca` replaces the OS roots, as redis-rs builds an empty store.
fn tls_certificates(
    conn: &ConnectionConfig,
    passphrase: Option<&Secret>,
) -> Result<Option<TlsCertificates>, ConnectError> {
    if conn.tls() != Some(TlsMode::Verify) {
        return Ok(None);
    }
    let root_cert = match conn.tls_ca() {
        Some(path) => {
            let fail = |e: String| tls_error(conn, format!("{}: {e}", path.display()));
            let bytes = std::fs::read(path).map_err(|e| fail(e.to_string()))?;
            let certs = CertificateDer::pem_slice_iter(&bytes)
                .collect::<Result<Vec<_>, _>>()
                .map_err(|e| fail(e.to_string()))?;
            if certs.is_empty() {
                return Err(fail("no certificates".to_owned()));
            }
            Some(bytes)
        }
        None => None,
    };
    // client_auth answers Some only when both paths are set.
    let client_tls = match (
        client_auth(conn, passphrase)?,
        conn.tls_cert(),
        conn.tls_key(),
    ) {
        (Some((chain, key)), Some(cert_path), Some(key_path)) => {
            let fail = |path: &std::path::Path, e: String| {
                tls_error(conn, format!("{}: {e}", path.display()))
            };
            let mut client_cert = String::new();
            for cert in &chain {
                client_cert.push_str(
                    &pem::encode_string("CERTIFICATE", LineEnding::LF, cert.as_ref())
                        .map_err(|e| fail(cert_path, e.to_string()))?,
                );
            }
            let client_key = key_pem(&key).map_err(|e| fail(key_path, e))?;
            Some(ClientTlsConfig {
                client_cert: client_cert.into_bytes(),
                client_key: client_key.into_bytes(),
            })
        }
        _ => None,
    };
    if root_cert.is_none() && client_tls.is_none() {
        return Ok(None);
    }
    Ok(Some(TlsCertificates {
        client_tls,
        root_cert,
    }))
}

/// `f`, or a failed connect when it has not finished within `CONNECT_TIMEOUT`.
async fn within<T>(target: &str, f: impl Future<Output = T>) -> Result<T, ConnectError> {
    tokio::time::timeout(CONNECT_TIMEOUT, f)
        .await
        .map_err(|_| ConnectError::Open {
            message: format!(
                "{target}: the server did not answer within {} s",
                CONNECT_TIMEOUT.as_secs()
            ),
        })
}

/// One connect-time call; an error reply fails the connect like a transport error, and an ACL
/// refusal names the `+grant` the user lacks.
async fn handshake(
    target: &str,
    conn: &mut MultiplexedConnection,
    cmd: &Cmd,
    grant: &str,
) -> Result<Value, ConnectError> {
    match within(target, conn.send_packed_command(cmd)).await? {
        Ok(Value::ServerError(e)) if e.code() == "NOPERM" => Err(ConnectError::Open {
            message: format!(
                "{target}: the user may not run {}, which Dbento needs: grant it with +{grant}",
                grant.to_uppercase()
            ),
        }),
        Ok(Value::ServerError(e)) => Err(ConnectError::Open {
            message: format!("{target}: {} {}", e.code(), e.details().unwrap_or_default()),
        }),
        Ok(value) => Ok(value),
        Err(e) => Err(open_error(target, e)),
    }
}

// The server's own words never hold the password, so `detail` is safe to show.
fn open_error(target: &str, e: RedisError) -> ConnectError {
    if let Some(code @ ("WRONGPASS" | "NOAUTH")) = e.code() {
        return ConnectError::Open {
            message: format!(
                "{target}: the server refused the login ({code}): {}",
                e.detail().unwrap_or_default()
            ),
        };
    }
    if e.kind() == ErrorKind::RESP3NotSupported {
        return ConnectError::Unsupported {
            reason: format!("{target}: {}", too_old("a version before 6.0")),
        };
    }
    ConnectError::Open {
        message: format!("{target}: {e}"),
    }
}

/// A failed dial: a rustls failure under `tls = "verify"` is `Tls` naming both ways out; anything
/// else is `Open` naming the target.
fn dial_error(conn: &ConnectionConfig, target: &str, e: RedisError) -> ConnectError {
    match rustls_cause(&e) {
        Some(cause) if conn.tls() == Some(TlsMode::Verify) => {
            tls_error(conn, format!("{target}: {cause}; {TLS_WAYS_OUT}"))
        }
        _ => open_error(target, e),
    }
}

/// The rustls error behind `e`, stored directly (a config failure) or inside an `io::Error`
/// (a handshake failure). `source()` alone hides both: it yields the `Arc` that holds them.
pub(super) fn rustls_cause(e: &RedisError) -> Option<&rustls::Error> {
    type Stored = std::sync::Arc<dyn std::error::Error + Send + Sync>;
    let inner: &(dyn std::error::Error + 'static) =
        &**std::error::Error::source(e)?.downcast_ref::<Stored>()?;
    inner.downcast_ref::<rustls::Error>().or_else(|| {
        inner
            .downcast_ref::<std::io::Error>()?
            .get_ref()?
            .downcast_ref::<rustls::Error>()
    })
}

#[cfg(test)]
mod tests;
