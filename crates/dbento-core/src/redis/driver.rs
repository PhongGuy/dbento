//! The Redis session: a `Driver` on tokio over one multiplexed connection, which the session drops
//! the moment the transport fails, ending it. Every statement is one raw command whose reply,
//! errors included, comes back as a value.

use std::collections::{BTreeSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use ::redis::aio::MultiplexedConnection;
use ::redis::{
    AsyncConnectionConfig, Client, Cmd, ConnectionAddr, ErrorKind, IntoConnectionInfo,
    ProtocolVersion, RedisConnectionInfo, RedisError, Value,
};
use tokio::sync::watch;

use super::classify::redis_classification;
use super::lex::{RedisStatement, SyntaxError, parse_script};
use super::probe::{gate, probe};
use super::shape::{ShapeError, reply_to_rows};
use super::stream::RedisStream;
use super::table::CommandTable;
use crate::Secret;
use crate::introspect::{ColumnMeta, IntrospectError, RelationList, RelationRef, SchemaInfo};
use crate::session::{
    CancelHandle, Classification, ConnectError, ConnectParams, Driver, ExecuteError, ExtendError,
    Flavor, Limits, Prepared, ReadOnlyEnforcement, ServerInfo, StreamItem, Writes,
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
}

/// Why a round trip did not answer.
#[derive(Debug)]
pub(crate) enum Failure {
    /// The session ended, now or earlier, for this reason.
    Ended(String),
}

impl From<Failure> for ExecuteError {
    fn from(failure: Failure) -> Self {
        let Failure::Ended(reason) = failure;
        ExecuteError::SessionEnded {
            reason: format!("{reason}; reopen the session"),
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
        conn.send_packed_command(cmd).await.map_err(|e| {
            self.end(format!("the connection failed: {e}"));
            Failure::Ended(e.to_string())
        })
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
}

pub(crate) struct RedisSession {
    shared: Arc<Shared>,
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
        .map_err(|e| open_error(&target, e))?;

        // redis-rs discards its own HELLO's reply, so the server is asked again.
        let hello = handshake(&target, &mut connection, ::redis::cmd("HELLO").arg(3)).await?;
        let server = server_info(&hello).map_err(|reason| ConnectError::Unsupported {
            reason: format!("{target}: {reason}"),
        })?;
        let reply = handshake(&target, &mut connection, &::redis::cmd("COMMAND")).await?;
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
                    other => other,
                })?;
            (Some(found.level), found.unprobeable)
        } else {
            (None, BTreeSet::new())
        };
        Ok(RedisSession {
            shared: Arc::new(Shared {
                conn: Mutex::new(Some(connection)),
                table,
                enforcement,
                unprobeable,
                ended: watch::Sender::new(None),
                settings_changed: AtomicBool::new(false),
            }),
            server,
            cancel: Arc::new(RedisCancel::new()),
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
        _limits: Limits,
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
        let affected = match reply {
            Value::Int(n) if writes => u64::try_from(n).ok(),
            _ => None,
        };
        let shaped = reply_to_rows(reply, None).map_err(shape_failure)?;
        Ok(RedisStream::new(shaped, affected))
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
        Err(IntrospectError::Unsupported {
            what: "the DB list",
        })
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

fn text_of(value: &Value) -> Option<String> {
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
    _passphrase: Option<&Secret>,
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
    let db = conn.database().unwrap_or("0");
    let db = db.parse::<i64>().map_err(|_| ConnectError::Open {
        message: format!("{}: `{db}` is not a DB index", conn.display_target()),
    })?;
    let mut settings = RedisConnectionInfo::default()
        .set_protocol(ProtocolVersion::RESP3)
        .set_db(db)
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
    Client::open(info).map_err(invalid)
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

/// One connect-time call; an error reply fails the connect like a transport error.
async fn handshake(
    target: &str,
    conn: &mut MultiplexedConnection,
    cmd: &Cmd,
) -> Result<Value, ConnectError> {
    match within(target, conn.send_packed_command(cmd)).await? {
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

#[cfg(test)]
mod tests;
