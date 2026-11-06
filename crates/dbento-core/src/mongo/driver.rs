//! The MongoDB session: a `Driver` on tokio with no connection thread, since the client,
//! the script's statements and the stream's cursor all live on the caller's runtime.

use std::collections::{BTreeSet, VecDeque};
use std::future::IntoFuture;
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use bson::{Bson, Document, RawDocumentBuf, doc};
use mongodb::Client;
use mongodb::action::Action;
use mongodb::error::{Error, ErrorKind};
use mongodb::options::{ClientOptions, Tls};
use tokio::sync::watch;

use super::catalog;
use super::classify::mongo_classification;
use super::columns::decode;
use super::exec::{
    command_error, read_preference, reply_failure, rows_affected, sendable, write_failure,
};
use super::parse::{MongoMethod, MongoOp, MongoStatement, SyntaxError, parse_script};
use super::stream::{MongoStream, counted, is_cursor, with_batch_size};
use crate::introspect::{ColumnMeta, IntrospectError, RelationList, RelationRef, SchemaInfo};
use crate::session::{
    CancelHandle, Classification, ConnectError, ConnectParams, DRAIN_CAP, Driver, ExecuteError,
    ExtendError, Flavor, Limits, Prepared, ReadOnlyEnforcement, ServerInfo, StreamItem, Writes,
};
use crate::value::ColumnInfo;
use crate::workspace::ConnectionConfig;

pub(crate) const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// How long past the statement timeout a cursor may wait, so the server's own code 50 wins when
/// it comes at all.
const DEADLINE_GRACE: Duration = Duration::from_secs(1);

/// The reader set of research A; a user holding only these has every write refused (research B).
pub(crate) const READER_ACTIONS: [&str; 11] = [
    "changeStream",
    "collStats",
    "dbHash",
    "dbStats",
    "find",
    "killCursors",
    "listCollections",
    "listDatabases",
    "listIndexes",
    "listSearchIndexes",
    "planCacheRead",
];

/// What the session, its script and its stream all see.
pub(crate) struct Shared {
    /// The current database: the URI's until a `use`.
    pub(super) database: Mutex<String>,
    pub(super) enforcement: Option<ReadOnlyEnforcement>,
    pub(super) timeout: Option<Duration>,
    pub(super) closed: AtomicBool,
    pub(super) settings_changed: AtomicBool,
}

pub(crate) struct MongoDriver {
    pub(super) client: Client,
    pub(super) server: ServerInfo,
    pub(super) uri_database: String,
    pub(super) shared: Arc<Shared>,
    pub(super) cancel: Arc<MongoCancel>,
}

/// The parsed statements of one script; `current` is the one prepared and not yet run.
pub(crate) struct MongoScript {
    client: Client,
    shared: Arc<Shared>,
    cancel: Arc<MongoCancel>,
    src: String,
    statements: VecDeque<Result<MongoStatement, SyntaxError>>,
    current: Option<(MongoStatement, Classification)>,
}

/// Cancels by generation: `cancel` bumps it, `settle` and a delivered cancel spend it.
#[derive(Debug)]
pub(crate) struct MongoCancel {
    generation: watch::Sender<u64>,
    spent: AtomicU64,
}

impl MongoCancel {
    pub(crate) fn new() -> Self {
        MongoCancel {
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

impl Shared {
    fn current_database(&self) -> MutexGuard<'_, String> {
        self.database.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The database `statement` runs on: its own `getSiblingDB`, else the current one.
    fn database_of(&self, statement: &MongoStatement) -> String {
        match &statement.db {
            Some(name) => name.clone(),
            None => self.current_database().clone(),
        }
    }

    /// What a failed command means; a failure that lost the server closes the session.
    pub(super) fn failure(&self, e: Error, writes: Writes) -> ExecuteError {
        let failure = command_error(e, writes, self.enforcement.is_some());
        if matches!(failure, ExecuteError::SessionClosed) {
            self.closed.store(true, Ordering::Relaxed);
        }
        failure
    }

    /// `f`, dropped at the statement timeout plus a grace. `maxTimeMS` does not reach the waits of
    /// a tailable cursor or a change stream, so only the client can end those. Dropping a cursor
    /// kills it. A `$out` or `$merge` aggregate does come through here, and `maxTimeMS` normally
    /// stops it first; past the grace it may have written part, as `Timeout`'s message says.
    pub(super) async fn deadline<F: Future>(&self, f: F) -> Result<F::Output, ExecuteError> {
        let Some(limit) = self.timeout else {
            return Ok(f.await);
        };
        tokio::time::timeout(limit + DEADLINE_GRACE, f)
            .await
            .map_err(|_| ExecuteError::Timeout)
    }
}

impl MongoDriver {
    pub(crate) fn server(&self) -> ServerInfo {
        self.server
    }

    /// How this session's writes are refused; `BestEffort` until privileges prove otherwise.
    pub(crate) fn enforcement(&self) -> ReadOnlyEnforcement {
        self.shared
            .enforcement
            .unwrap_or(ReadOnlyEnforcement::BestEffort)
    }

    pub(crate) fn settings_changed(&self) -> bool {
        self.shared.settings_changed.load(Ordering::Relaxed)
    }
}

impl Driver for MongoDriver {
    type Script = MongoScript;
    type Stream = MongoStream;

    async fn connect(params: ConnectParams<'_>) -> Result<Self, ConnectError> {
        let conn = params.conn;
        let fail = |e| open_error(conn, e);
        // The workspace already refused a `uri` holding a password, so neither this text nor
        // the error built from it can carry one.
        let mut options = ClientOptions::parse(conn.uri().unwrap_or_default())
            .await
            .map_err(fail)?;
        options.app_name = Some("dbento".to_owned());
        let limit = fill_timeouts(&mut options);
        if let Some(credential) = &mut options.credential {
            credential.password = params.secret.map(|s| s.expose().to_owned());
        }
        resolve_tls_paths(&mut options, params.ws_dir);
        let client = Client::with_options(options).map_err(fail)?;

        // These commands need no privileges, so an unauthenticated session gets this far and
        // its empty privilege list reads as `BestEffort`.
        let admin = client.database("admin");
        let handshake = async {
            admin.run_command(doc! { "ping": 1 }).await.map_err(fail)?;
            let build = admin
                .run_command(doc! { "buildInfo": 1 })
                .await
                .map_err(fail)?;
            let enforcement = if params.read_only {
                let status = admin
                    .run_command(doc! { "connectionStatus": 1, "showPrivileges": true })
                    .await
                    .map_err(fail)?;
                Some(enforcement_of(&privilege_actions(&status)))
            } else {
                None
            };
            Ok((build, enforcement))
        };
        let (build, enforcement) = within(limit, conn.display_target(), handshake).await?;

        let text = build.get_str("version").ok();
        let version = text
            .and_then(version_of)
            .ok_or_else(|| ConnectError::Open {
                message: format!(
                    "{}: unexpected server version {text:?}",
                    conn.display_target()
                ),
            })?;
        let database = conn.database().unwrap_or_default().to_owned();
        Ok(MongoDriver {
            client,
            server: ServerInfo {
                flavor: Flavor::MongoDb,
                version,
            },
            uri_database: database.clone(),
            shared: Arc::new(Shared {
                database: Mutex::new(database),
                enforcement,
                timeout: params.timeout,
                closed: AtomicBool::new(false),
                settings_changed: AtomicBool::new(false),
            }),
            cancel: Arc::new(MongoCancel::new()),
        })
    }

    fn start_script(&mut self, sql: &str) -> MongoScript {
        MongoScript {
            client: self.client.clone(),
            shared: Arc::clone(&self.shared),
            cancel: Arc::clone(&self.cancel),
            src: sql.to_owned(),
            statements: parse_script(sql).into(),
            current: None,
        }
    }

    async fn prepare_next(script: &mut MongoScript) -> Option<Result<Prepared, ExecuteError>> {
        let next = script.statements.pop_front()?;
        let parsed = if script.shared.closed.load(Ordering::Relaxed) {
            Err(ExecuteError::SessionClosed)
        } else {
            next.map_err(|e| ExecuteError::Syntax {
                message: e.message,
                offset: e.offset,
            })
        };
        let statement = match parsed {
            Ok(statement) => statement,
            Err(failure) => {
                // A failure ends the script, so a caller that keeps asking never reaches a
                // later statement.
                script.statements.clear();
                return Some(Err(failure));
            }
        };
        let classification = mongo_classification(&statement.op);
        let prepared = Prepared {
            sql: script.src[statement.range.clone()].to_owned(),
            classification: classification.clone(),
        };
        script.current = Some((statement, classification));
        Some(Ok(prepared))
    }

    async fn is_last(script: &mut MongoScript) -> bool {
        script.statements.is_empty()
    }

    async fn execute(
        script: &mut MongoScript,
        limits: Limits,
    ) -> Result<(Vec<ColumnInfo>, MongoStream), ExecuteError> {
        let (statement, classification) =
            script.current.take().ok_or(ExecuteError::SessionClosed)?;
        // The gate sits here, not in `prepare_next`, so a shell can read a refused statement's
        // classification.
        if let Some(refusal) = refused(&classification, script.shared.enforcement) {
            return Err(refusal);
        }
        if is_cursor(&statement.op) {
            let command = sendable(&statement.op, script.shared.timeout)?;
            let database = script
                .client
                .database(&script.shared.database_of(&statement));
            return MongoStream::open(
                &database,
                with_batch_size(command, limits),
                limits,
                classification.writes,
                Arc::clone(&script.shared),
                Arc::clone(&script.cancel),
            )
            .await;
        }
        let affected = match &statement.op {
            MongoOp::Use(name) => {
                *script.shared.current_database() = name.clone();
                script
                    .shared
                    .settings_changed
                    .store(true, Ordering::Relaxed);
                None
            }
            MongoOp::Method { method, .. } if reports_a_count(*method) => {
                Some(write(script, &statement, classification.writes).await?)
            }
            _ => return reply_rows(script, &statement, limits, classification.writes).await,
        };
        let stream = MongoStream::finished(
            affected,
            Arc::clone(&script.shared),
            Arc::clone(&script.cancel),
        );
        Ok((Vec::new(), stream))
    }

    async fn next_event(stream: &mut MongoStream) -> Result<StreamItem, ExecuteError> {
        stream.next_event().await
    }

    fn extend(stream: &mut MongoStream, more: Option<u64>) -> Result<(), ExtendError> {
        stream.extend(more)
    }

    fn cancel_handle(&self) -> CancelHandle {
        CancelHandle::mongo(Arc::clone(&self.cancel))
    }

    async fn settle(&mut self) -> Result<(), ExecuteError> {
        if self.shared.closed.load(Ordering::Relaxed) {
            return Err(ExecuteError::SessionClosed);
        }
        self.cancel.spend();
        Ok(())
    }

    async fn close(self) -> Result<(), ExecuteError> {
        // A graceful shutdown lets a just-dropped cursor's `killCursors` finish; past the cap
        // the session ends anyway and the server reaps what is left.
        let _ = tokio::time::timeout(DRAIN_CAP, self.client.shutdown()).await;
        Ok(())
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

    fn read_only_level(&self, requested: bool) -> Option<ReadOnlyEnforcement> {
        requested.then(|| self.enforcement())
    }
}

/// Sends a write or DDL command and reads what it changed. An `ok: 1` reply can still carry a
/// failure, and a failure that loses the server closes the session.
async fn write(
    script: &MongoScript,
    statement: &MongoStatement,
    writes: Writes,
) -> Result<u64, ExecuteError> {
    let reply = send(script, statement, writes).await?;
    match write_failure(&statement.op, &reply) {
        Some(failure) => Err(failure),
        None => Ok(rows_affected(&reply)),
    }
}

async fn send(
    script: &MongoScript,
    statement: &MongoStatement,
    writes: Writes,
) -> Result<Document, ExecuteError> {
    let shared = &script.shared;
    let command = sendable(&statement.op, shared.timeout)?;
    let database = script.client.database(&shared.database_of(statement));
    let call = database
        .run_command(command)
        .optional(read_preference(&database, writes), |call, criteria| {
            call.selection_criteria(criteria)
        });
    script
        .cancel
        .guard(call.into_future())
        .await?
        .map_err(|e| shared.failure(e, writes))
}

/// Runs a statement whose reply is its result, and streams that reply as a row.
async fn reply_rows(
    script: &MongoScript,
    statement: &MongoStatement,
    limits: Limits,
    writes: Writes,
) -> Result<(Vec<ColumnInfo>, MongoStream), ExecuteError> {
    let shared = &script.shared;
    let document = if let MongoOp::Method {
        method: MongoMethod::CountDocuments,
        ..
    } = statement.op
    {
        let command = sendable(&statement.op, shared.timeout)?;
        let database = script.client.database(&shared.database_of(statement));
        let group = script
            .cancel
            .guard(shared.deadline(counted(&database, command, writes)))
            .await??
            .map_err(|e| shared.failure(e, writes))?;
        let n = group.as_deref().map(decode).transpose()?;
        let n = n.and_then(|mut g| g.remove("n")).unwrap_or(Bson::Int32(0));
        Some(doc! { "n": n })
    } else {
        shown(&statement.op, send(script, statement, writes).await?)?
    };
    let document = document
        .map(|d| RawDocumentBuf::try_from(&d))
        .transpose()
        .map_err(|e| ExecuteError::database("driver", e.to_string()))?;
    MongoStream::reply(
        document,
        limits,
        writes,
        Arc::clone(shared),
        Arc::clone(&script.cancel),
    )
}

/// What `reply` shows as a row, if anything. A shell method fails on a failure inside an `ok: 1`
/// reply, while `db.runCommand` shows that reply as mongosh does, except a timeout.
fn shown(op: &MongoOp, mut reply: Document) -> Result<Option<Document>, ExecuteError> {
    use MongoMethod::*;
    let failure = reply_failure(&reply);
    let failure = match op {
        MongoOp::RunCommand(_) => failure.filter(|f| matches!(f, ExecuteError::Timeout)),
        _ => failure,
    };
    if let Some(failure) = failure {
        return Err(failure);
    }
    let MongoOp::Method {
        method: FindOneAndUpdate | FindOneAndReplace | FindOneAndDelete,
        ..
    } = op
    else {
        return Ok(Some(reply));
    };
    Ok(match reply.remove("value") {
        Some(Bson::Document(document)) => Some(document),
        _ => None,
    })
}

// `findOneAnd*` also writes, but it answers with a document, not a count.
fn reports_a_count(method: MongoMethod) -> bool {
    use MongoMethod::*;
    matches!(
        method,
        InsertOne
            | InsertMany
            | UpdateOne
            | UpdateMany
            | ReplaceOne
            | DeleteOne
            | DeleteMany
            | CreateIndex
            | DropIndex
            | Drop
            | CreateCollection
            | DropDatabase
    )
}

/// Why a read-only session refuses this statement, if it does. Only `Database` lets `Unknown`
/// through, because there the server is the fence; any other level fails closed.
fn refused(
    classification: &Classification,
    enforcement: Option<ReadOnlyEnforcement>,
) -> Option<ExecuteError> {
    let message = match (enforcement?, &classification.writes) {
        (_, Writes::No) | (ReadOnlyEnforcement::Database, Writes::Unknown) => return None,
        (_, Writes::Yes) => "this statement writes",
        (_, Writes::Unknown) => {
            "Dbento cannot tell whether this command writes, and this connection's user can write"
        }
    };
    Some(ExecuteError::ReadOnlyViolation {
        message: message.to_owned(),
    })
}

/// Fills the timeouts the URI left unset, whose own values stand, and gives the bound on the
/// handshake: long enough for server selection, plus a server's slowest answer.
fn fill_timeouts(options: &mut ClientOptions) -> Duration {
    let selection = *options
        .server_selection_timeout
        .get_or_insert(CONNECT_TIMEOUT);
    options.connect_timeout.get_or_insert(CONNECT_TIMEOUT);
    selection + CONNECT_TIMEOUT
}

/// `f`, or a failed connect if it has not finished within `limit`.
async fn within<T>(
    limit: Duration,
    target: String,
    f: impl Future<Output = Result<T, ConnectError>>,
) -> Result<T, ConnectError> {
    tokio::time::timeout(limit, f)
        .await
        .map_err(|_| ConnectError::Open {
            message: format!(
                "{target}: the server did not answer within {} s",
                limit.as_secs()
            ),
        })?
}

// Reads `e.kind`, not `e`, because `Error`'s `Display` dumps the server's reply as hex.
fn open_error(conn: &ConnectionConfig, e: Error) -> ConnectError {
    let target = conn.display_target();
    match &*e.kind {
        ErrorKind::InvalidTlsConfig { .. } => ConnectError::Tls {
            host: target,
            message: e.kind.to_string(),
        },
        ErrorKind::Authentication { .. } => ConnectError::Open {
            message: format!(
                "{target}: {}; check the user and `authSource` in `uri`",
                e.kind
            ),
        },
        _ => ConnectError::Open {
            message: format!("{target}: {}", e.kind),
        },
    }
}

/// Every action `connectionStatus` granted, on any resource; a reply without the list has none.
pub(crate) fn privilege_actions(reply: &Document) -> BTreeSet<String> {
    let privileges = reply
        .get_document("authInfo")
        .and_then(|info| info.get_array("authenticatedUserPrivileges"));
    privileges
        .into_iter()
        .flatten()
        .filter_map(|privilege| privilege.as_document()?.get_array("actions").ok())
        .flatten()
        .filter_map(|action| action.as_str().map(str::to_owned))
        .collect()
}

/// `Database` only when the server is known to refuse every write: a non-empty list wholly inside
/// the reader set. An action this list does not know might write, so it counts as one.
pub(crate) fn enforcement_of(actions: &BTreeSet<String>) -> ReadOnlyEnforcement {
    let reader = |action: &String| READER_ACTIONS.contains(&action.as_str());
    if !actions.is_empty() && actions.iter().all(reader) {
        ReadOnlyEnforcement::Database
    } else {
        ReadOnlyEnforcement::BestEffort
    }
}

/// Major, minor and patch of `buildInfo`'s version, such as "8.0.34" or "7.0.2-rc1".
pub(crate) fn version_of(text: &str) -> Option<(u16, u16, u16)> {
    let mut parts = text.split(['.', '-']).map(|p| p.parse::<u16>().ok());
    Some((parts.next()??, parts.next()??, parts.next()??))
}

/// Points relative certificate paths at the workspace folder; the URI text stays as written.
pub(crate) fn resolve_tls_paths(options: &mut ClientOptions, dir: &Path) {
    if let Some(Tls::Enabled(tls)) = &mut options.tls {
        let paths = [&mut tls.ca_file_path, &mut tls.cert_key_file_path];
        // `join` keeps an absolute path as it is.
        for path in paths.into_iter().flatten() {
            *path = dir.join(&*path);
        }
    }
}

#[cfg(test)]
mod tests;
