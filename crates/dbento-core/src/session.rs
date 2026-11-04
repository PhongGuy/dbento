//! The public session API: connect, run scripts statement by statement, stream results.
//!
//! Public types are thin wrappers that dispatch over a crate-internal enum with one variant
//! per engine; each engine implements [`Driver`].

use std::marker::PhantomData;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use crate::Secret;
#[cfg(feature = "duckdb")]
use crate::duckdb::{DuckCancel, DuckDriver};
use crate::explain::{ExplainOutput, ExplainRequest};
use crate::fkeys::{ForeignKey, pg_foreign_keys, sort_keys, sqlite_foreign_keys};
use crate::introspect::{ColumnMeta, IntrospectError, RelationList, RelationRef, SchemaInfo};
#[cfg(feature = "mongodb")]
use crate::mongo::driver::{MongoCancel, MongoDriver};
use crate::mssql::{MssqlCancel, MssqlDriver};
use crate::mysql::{MysqlCancel, MysqlDriver};
use crate::objects::{
    Definition, DefinitionError, ObjectRef, RelationObjects, SchemaObjects, pg_definition,
    pg_relation_objects, pg_schema_objects, sqlite_definition, sqlite_relation_objects,
};
use crate::postgres::{PgCancel, PgDriver};
use crate::rowedit::{EditTarget, EditValue, Param, pg_edit_target, sqlite_edit_target};
use crate::sqlite::{SqliteCancel, SqliteDriver};
use crate::value::{ColumnInfo, ResultBatch};
use crate::workspace::{ConnectionConfig, Engine, Workspace};

/// Per-session overrides of the connection's configuration.
#[non_exhaustive]
#[derive(Debug, Clone, Default)]
pub struct SessionOptions {
    /// Run the session read-only even when the connection allows writes.
    pub force_read_only: bool,
    /// Overrides the connection's `statement_timeout`.
    pub statement_timeout: Option<Duration>,
    /// Decrypts an encrypted `tls_key`; see [`crate::resolve_key_passphrase`].
    pub key_passphrase: Option<Secret>,
    /// The session runs only SQL that Dbento generated, so a database may enforce read-only
    /// by transaction alone. Other engines ignore it.
    pub trusted_sql: bool,
}

/// What to do with the rest of a result once a row or byte bound is reached.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum OnTruncate {
    /// Discard the remaining rows and keep the session.
    #[default]
    Drain,
    /// Close the session at once; later calls return [`ExecuteError::SessionClosed`].
    Close,
    /// Keep the statement open at a row cut; see [`ResultStream::extend`]. A byte cut drains.
    Park,
}

/// Why [`ResultStream::extend`] failed.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum ExtendError {
    /// The stream is not parked at its row cap.
    #[error("the result is not parked at its row limit")]
    NotParked,
    /// The session's connection is gone.
    #[error("the session is closed")]
    SessionClosed,
    /// A cut-short statement was still running past the drain cap, so it was stopped and its
    /// connection dropped; the session is closed. A transactional write is rolled back; a
    /// non-transactional one (such as MySQL MyISAM) may keep what it wrote before the stop.
    #[error(
        "the statement was still running when the row limit was hit, \
         so it was stopped and any writes it made were rolled back"
    )]
    StoppedAtDrainCap,
}

/// Limits applied to one statement's result.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, Default)]
pub struct Limits {
    /// Stop collecting after this many rows; `None` collects everything.
    pub max_rows: Option<u64>,
    /// Stop once the rows delivered hold at least this many bytes of cell data and another
    /// row exists; `None` collects everything. NULL counts 0, text and bytes their length.
    pub max_bytes: Option<u64>,
    /// What happens to the rest of a truncated result.
    pub on_truncate: OnTruncate,
}

/// How a statement ended.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// Rows delivered.
    pub rows: u64,
    /// Whether more rows existed than were delivered.
    pub truncated: bool,
    /// Rows changed, for a statement that returned no columns; `None` otherwise.
    pub rows_affected: Option<u64>,
    /// Which bound cut the result; `Some` exactly when `truncated`.
    pub truncated_by: Option<TruncatedBy>,
    /// Result sets after the first that have columns, counted only for a result read to its
    /// end; always 0 on Postgres and SQLite.
    pub extra_results: u32,
    /// The statement's changes did not persist: a transaction was rolled back around it.
    /// False on every engine but SurrealDB.
    pub rolled_back: bool,
}

/// The bound that cut a result short.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TruncatedBy {
    /// `Limits::max_rows` was reached.
    Rows,
    /// `Limits::max_bytes` was reached.
    Bytes,
}

impl Limits {
    /// The bound that stops the next row, given the rows delivered and their cell bytes.
    /// The row bound is checked first, so a tie is `Rows`.
    pub(crate) fn cut(&self, delivered: u64, bytes: u64) -> Option<TruncatedBy> {
        if self.max_rows == Some(delivered) {
            Some(TruncatedBy::Rows)
        } else if self.max_bytes.is_some_and(|b| bytes >= b) {
            Some(TruncatedBy::Bytes)
        } else {
            None
        }
    }
}

/// Whether a statement writes, as far as the engine's parser can tell.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Writes {
    /// The statement only reads.
    No,
    /// The statement may write.
    Yes,
    /// The parser could not handle the statement.
    Unknown,
}

/// A statement that touches every row of a table or removes an object.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unguarded {
    /// `UPDATE` without `WHERE`.
    UpdateWithoutWhere,
    /// `DELETE` without `WHERE`.
    DeleteWithoutWhere,
    /// A `DROP` of any object.
    Drop,
    /// `TRUNCATE`.
    Truncate,
    /// `updateMany` with an empty filter.
    UpdateAllDocuments,
    /// `deleteMany` with an empty filter.
    DeleteAllDocuments,
}

impl Unguarded {
    /// The short name every shell shows for the kind, such as "UPDATE without WHERE".
    pub fn label(self) -> &'static str {
        match self {
            Self::UpdateWithoutWhere => "UPDATE without WHERE",
            Self::DeleteWithoutWhere => "DELETE without WHERE",
            Self::Drop => "DROP",
            Self::Truncate => "TRUNCATE",
            Self::UpdateAllDocuments => "updateMany with an empty filter",
            Self::DeleteAllDocuments => "deleteMany with an empty filter",
        }
    }
}

/// What a statement does, known before it runs.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Classification {
    /// Whether the statement writes.
    pub writes: Writes,
    /// Every unguarded change in the statement, in the order found.
    pub unguarded: Vec<Unguarded>,
    /// The statement begins, ends or changes a transaction or savepoint.
    pub transaction_control: bool,
}

/// What the session's engine supports.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capabilities {
    /// The engine has schemas inside a database.
    pub schemas: bool,
    /// A cancel stops the statement inside the engine.
    pub server_cancel: bool,
    /// The connection authenticates with a secret from the keychain or the environment.
    pub password: bool,
    /// The connection is a database file path.
    pub file: bool,
    /// The TLS keys this engine's form shows apply.
    pub tls: bool,
    /// Staged row edits can be applied.
    pub edit_rows: bool,
    /// EXPLAIN plans can be requested.
    pub explain: bool,
    /// A table has its CREATE text in the schema tree.
    pub table_definition: bool,
    /// The language the engine's query text is written in.
    pub language: QueryLanguage,
}

/// The language a connection's query text is written in.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QueryLanguage {
    /// SQL.
    Sql,
    /// The mongosh statement subset.
    MongoShell,
    /// SurrealQL.
    SurrealQl,
}

impl QueryLanguage {
    /// The name shells show for the language: "SQL", "MongoDB shell" or "SurrealQL".
    pub fn name(self) -> &'static str {
        match self {
            Self::Sql => "SQL",
            Self::MongoShell => "MongoDB shell",
            Self::SurrealQl => "SurrealQL",
        }
    }

    /// The saved-query file extension, without the dot: "sql", "js" or "surql".
    pub fn file_extension(self) -> &'static str {
        match self {
            Self::Sql => "sql",
            Self::MongoShell => "js",
            Self::SurrealQl => "surql",
        }
    }
}

/// Which server of a family a session talks to.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flavor {
    /// PostgreSQL.
    Postgres,
    /// Oracle MySQL.
    Mysql,
    /// MariaDB.
    Mariadb,
    /// Microsoft SQL Server.
    SqlServer,
    /// MongoDB.
    MongoDb,
    /// SurrealDB.
    SurrealDb,
}

/// The server a session is connected to, as its handshake reported it.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServerInfo {
    /// The server family member.
    pub flavor: Flavor,
    /// Major, minor and patch version.
    pub version: (u16, u16, u16),
}

/// Why [`connect`] failed.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum ConnectError {
    /// The SQLite or DuckDB database file is missing; it is never created.
    #[error("database file {} does not exist", path.display())]
    NotFound {
        /// The missing file.
        path: PathBuf,
    },
    /// The engine refused to open the database.
    #[error("cannot open the database: {message}")]
    Open {
        /// The engine's message.
        message: String,
    },
    /// TLS could not be set up or the server's certificate failed verification.
    #[error("TLS with {host} failed: {message}")]
    Tls {
        /// The host as configured.
        host: String,
        /// What failed.
        message: String,
    },
    /// The effective statement timeout is zero or longer than Postgres accepts.
    #[error(
        "statement timeout {} must be above zero and at most 2147483647ms",
        humantime::format_duration(*timeout)
    )]
    InvalidTimeout {
        /// The rejected timeout.
        timeout: Duration,
    },
    /// The connection uses something this build cannot do yet.
    #[error("unsupported: {reason}")]
    Unsupported {
        /// What is unsupported.
        reason: String,
    },
}

/// Why a statement failed.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum ExecuteError {
    /// The database reported an error.
    #[error("{message} ({code})")]
    #[non_exhaustive]
    Database {
        /// SQLSTATE on Postgres, the extended result code in decimal on SQLite, the error
        /// number in decimal on MySQL and SQL Server.
        code: String,
        /// The server's SQLSTATE on MySQL; `None` on Postgres, SQLite and SQL Server.
        sqlstate: Option<String>,
        /// The error's class (severity) on SQL Server; `None` elsewhere.
        class: Option<u8>,
        /// The error's state on SQL Server; `None` elsewhere.
        state: Option<u8>,
        /// The database's message.
        message: String,
        /// The server's detail line (Postgres).
        detail: Option<String>,
        /// The server's hint (Postgres).
        hint: Option<String>,
        /// 0-based byte offset into the statement: the text as sent on Postgres, the trimmed
        /// `statement_ranges` range text on SQLite.
        position: Option<usize>,
    },
    /// The statement ran past the statement timeout.
    #[error("the statement timed out")]
    Timeout,
    /// The statement was cancelled through its session's cancel handle.
    #[error("the statement was cancelled")]
    Cancelled,
    /// A read-only session refused a write.
    #[error("read-only session: {message}")]
    ReadOnlyViolation {
        /// The database's message, or why Dbento refused or reported the batch.
        message: String,
    },
    /// The statement is refused without running.
    #[error("unsupported: {reason}")]
    Unsupported {
        /// Why it is refused.
        reason: String,
    },
    /// A read-only session refused transaction control; each statement runs in its own
    /// read-only transaction.
    #[error("{reason}")]
    TransactionControl {
        /// Why it is refused.
        reason: String,
    },
    /// The session's connection is gone.
    #[error("the session is closed")]
    SessionClosed,
    /// A cut-short statement was still running past the drain cap, so it was stopped and its
    /// connection dropped; the session is closed. A transactional write is rolled back; a
    /// non-transactional one (such as MySQL MyISAM) may keep what it wrote before the stop.
    #[error(
        "the statement was still running when the row limit was hit, \
         so it was stopped and any writes it made were rolled back"
    )]
    StoppedAtDrainCap,
    /// Dbento's own parser refused the statement.
    #[error("syntax error at byte {offset}: {message}")]
    Syntax {
        /// What is wrong.
        message: String,
        /// 0-based byte offset into the statement.
        offset: usize,
    },
    /// A parked result was given up by the server or by its bound; the session stays open.
    #[error("the result expired; run the query again")]
    ResultExpired,
}

impl ExecuteError {
    /// The error's text for a session on `engine`: MySQL's drain-cap stop warns that a
    /// non-transactional write may be kept, SQL Server's that the batch's finished statements
    /// keep their writes, and MongoDB's timeout and cancel that a write has no transaction to
    /// undo it; everything else is the `Display` text.
    pub fn message_for(&self, engine: Engine) -> String {
        match (self, engine) {
            (ExecuteError::StoppedAtDrainCap, Engine::Mysql) => {
                "the statement was still running when the row limit was hit, so it was stopped; \
                 a transactional write was rolled back, but a non-transactional one may have \
                 kept part of its changes"
                    .to_owned()
            }
            (ExecuteError::StoppedAtDrainCap, Engine::Mssql) => {
                "the statement was still running when its result was cut short or abandoned, so \
                 it was stopped and the session closed; statements of the batch that already \
                 finished keep what they wrote, and only an open transaction was rolled back"
                    .to_owned()
            }
            (ExecuteError::Timeout, Engine::Mongodb) => {
                "the statement timed out; a write may have kept part of its changes".to_owned()
            }
            (ExecuteError::Cancelled, Engine::Mongodb) => {
                "the statement was cancelled here; a write may keep running on the server until \
                 it ends or hits the time limit"
                    .to_owned()
            }
            // Attention ends the batch; what it already ran stays unless a transaction undoes it.
            (ExecuteError::Cancelled | ExecuteError::Timeout, Engine::Mssql) => format!(
                "{self}; statements of the batch that already finished may have kept what \
                 they wrote, even when their row counts never arrived"
            ),
            _ => self.to_string(),
        }
    }

    /// True when the session can run nothing more, whatever the reason it closed.
    pub fn closes_session(&self) -> bool {
        matches!(
            self,
            ExecuteError::SessionClosed | ExecuteError::StoppedAtDrainCap
        )
    }

    /// A database error with only a code and a message; every other field is empty.
    pub fn database(code: impl Into<String>, message: impl Into<String>) -> Self {
        ExecuteError::Database {
            code: code.into(),
            sqlstate: None,
            class: None,
            state: None,
            message: message.into(),
            detail: None,
            hint: None,
            position: None,
        }
    }
}

/// How long settling may spend on a truncated, dropped or abandoned result.
pub(crate) const DRAIN_CAP: Duration = Duration::from_secs(10);

/// A statement prepared by a driver and not yet run.
pub(crate) struct Prepared {
    pub(crate) sql: String,
    pub(crate) classification: Classification,
}

/// One event of a running statement.
pub(crate) enum StreamItem {
    Batch(ResultBatch),
    Done(Outcome),
}

/// What a driver needs to open a session: the connection, its resolved secret and the
/// effective read-only flag and statement timeout.
pub(crate) struct ConnectParams<'a> {
    pub(crate) conn: &'a ConnectionConfig,
    pub(crate) secret: Option<&'a Secret>,
    pub(crate) key_passphrase: Option<&'a Secret>,
    pub(crate) read_only: bool,
    pub(crate) timeout: Option<Duration>,
    /// See [`SessionOptions::trusted_sql`].
    pub(crate) trusted_sql: bool,
    /// The workspace folder, which relative certificate paths in a MongoDB `uri` resolve from.
    #[cfg_attr(not(feature = "mongodb"), allow(dead_code))] // only MongoDB reads it
    pub(crate) ws_dir: &'a Path,
}

/// The engine-specific half of a session. Each future it returns is `Send` because the
/// implementing types are.
pub(crate) trait Driver: Sized + Send {
    type Script: Send;
    type Stream: Send;
    async fn connect(params: ConnectParams<'_>) -> Result<Self, ConnectError>;
    fn start_script(&mut self, sql: &str) -> Self::Script;
    async fn prepare_next(script: &mut Self::Script) -> Option<Result<Prepared, ExecuteError>>;
    async fn is_last(script: &mut Self::Script) -> bool;
    async fn execute(
        script: &mut Self::Script,
        limits: Limits,
    ) -> Result<(Vec<ColumnInfo>, Self::Stream), ExecuteError>;
    async fn next_event(stream: &mut Self::Stream) -> Result<StreamItem, ExecuteError>;
    /// Raises a parked stream's row cap by `more`, or lifts it for `None`; does no I/O.
    fn extend(stream: &mut Self::Stream, more: Option<u64>) -> Result<(), ExtendError>;
    fn cancel_handle(&self) -> CancelHandle;
    async fn settle(&mut self) -> Result<(), ExecuteError>;
    async fn close(self) -> Result<(), ExecuteError>;
    async fn schemas(&mut self) -> Result<Vec<SchemaInfo>, IntrospectError>;
    async fn relations(&mut self, schema: &str) -> Result<RelationList, IntrospectError>;
    async fn columns(&mut self, rel: &RelationRef) -> Result<Vec<ColumnMeta>, IntrospectError>;
    /// The session's read-only level once connected, `None` when writable. `requested` is the
    /// connect argument; a driver overrides this when its login decides.
    fn read_only_level(&self, requested: bool) -> Option<ReadOnlyEnforcement> {
        requested.then_some(ReadOnlyEnforcement::Database)
    }
}

enum SessionInner {
    Sqlite(SqliteDriver),
    Postgres(PgDriver),
    Mysql(MysqlDriver),
    Mssql(MssqlDriver),
    #[cfg(feature = "duckdb")]
    Duckdb(DuckDriver),
    #[cfg(feature = "mongodb")]
    Mongodb(MongoDriver),
}

enum ScriptInner {
    Sqlite(<SqliteDriver as Driver>::Script),
    Postgres(<PgDriver as Driver>::Script),
    Mysql(<MysqlDriver as Driver>::Script),
    Mssql(<MssqlDriver as Driver>::Script),
    #[cfg(feature = "duckdb")]
    Duckdb(<DuckDriver as Driver>::Script),
    // Boxed, like its stream, so every engine's `Script` does not carry the parsed statements.
    #[cfg(feature = "mongodb")]
    Mongodb(Box<<MongoDriver as Driver>::Script>),
}

enum StreamInner {
    Sqlite(<SqliteDriver as Driver>::Stream),
    Postgres(<PgDriver as Driver>::Stream),
    Mysql(<MysqlDriver as Driver>::Stream),
    Mssql(<MssqlDriver as Driver>::Stream),
    #[cfg(feature = "duckdb")]
    Duckdb(<DuckDriver as Driver>::Stream),
    // Boxed so the driver's large cursor does not set the size of every engine's stream.
    #[cfg(feature = "mongodb")]
    Mongodb(Box<<MongoDriver as Driver>::Stream>),
}

/// Opens a session on `conn`. `secret` is the resolved password, if the engine needs one.
pub async fn connect(
    ws: &Workspace,
    conn: &ConnectionConfig,
    secret: Option<Secret>,
    opts: SessionOptions,
) -> Result<Session, ConnectError> {
    let read_only = opts.force_read_only || conn.read_only();
    let timeout = opts.statement_timeout.or(conn.statement_timeout());
    // The options bypass the workspace's check, so the effective value is checked here.
    if let Some(t) = timeout.filter(|t| !crate::workspace::timeout_in_range(*t)) {
        return Err(ConnectError::InvalidTimeout { timeout: t });
    }
    let params = ConnectParams {
        conn,
        secret: secret.as_ref(),
        key_passphrase: opts.key_passphrase.as_ref(),
        read_only,
        timeout,
        trusted_sql: opts.trusted_sql,
        ws_dir: ws.dir(),
    };
    match conn.engine() {
        Engine::Postgres => {
            let d = PgDriver::connect(params).await?;
            let level = d.read_only_level(read_only);
            Ok(Session {
                inner: SessionInner::Postgres(d),
                read_only: level,
            })
        }
        Engine::Mysql => {
            let d = MysqlDriver::connect(params).await?;
            let level = d.read_only_level(read_only);
            Ok(Session {
                inner: SessionInner::Mysql(d),
                read_only: level,
            })
        }
        Engine::Sqlite => {
            let d = SqliteDriver::connect(params).await?;
            let level = d.read_only_level(read_only);
            Ok(Session {
                inner: SessionInner::Sqlite(d),
                read_only: level,
            })
        }
        Engine::Mssql => {
            let d = MssqlDriver::connect(params).await?;
            let level = d.read_only_level(read_only);
            Ok(Session {
                inner: SessionInner::Mssql(d),
                read_only: level,
            })
        }
        #[cfg(feature = "duckdb")]
        Engine::Duckdb => {
            let d = DuckDriver::connect(params).await?;
            let level = d.read_only_level(read_only);
            Ok(Session {
                inner: SessionInner::Duckdb(d),
                read_only: level,
            })
        }
        #[cfg(not(feature = "duckdb"))]
        Engine::Duckdb => Err(ConnectError::Unsupported {
            reason: "DuckDB is not available in this build".to_owned(),
        }),
        #[cfg(feature = "mongodb")]
        Engine::Mongodb => {
            let d = MongoDriver::connect(params).await?;
            let level = d.read_only_level(read_only);
            Ok(Session {
                inner: SessionInner::Mongodb(d),
                read_only: level,
            })
        }
        #[cfg(not(feature = "mongodb"))]
        Engine::Mongodb => Err(ConnectError::Unsupported {
            reason: "MongoDB is not available in this build".to_owned(),
        }),
        #[cfg(feature = "surrealdb")]
        Engine::Surrealdb => Err(ConnectError::Unsupported {
            reason: "SurrealDB sessions are not available yet".to_owned(),
        }),
        #[cfg(not(feature = "surrealdb"))]
        Engine::Surrealdb => Err(ConnectError::Unsupported {
            reason: "SurrealDB is not available in this build".to_owned(),
        }),
    }
}

/// How far a read-only session's refusal of writes can be trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReadOnlyEnforcement {
    /// The database refuses every write: Postgres, MySQL, SQLite, DuckDB, SQL Server with
    /// read_only_user, MongoDB with a user whose privileges only read, and SurrealDB's fence for
    /// a VIEWER login.
    Database,
    /// SQL Server without read_only_user: Dbento's fence and refusals, with known holes; MongoDB
    /// with any other user, where Dbento refuses every statement it cannot prove reads; and
    /// SurrealDB's fence for root OWNER, EDITOR or anonymous, where stored code can reach what
    /// the fence cannot undo.
    BestEffort,
}

/// An open connection to one database.
pub struct Session {
    inner: SessionInner,
    read_only: Option<ReadOnlyEnforcement>,
}

impl Session {
    /// The session's engine.
    pub fn engine(&self) -> Engine {
        match self.inner {
            SessionInner::Sqlite(_) => Engine::Sqlite,
            SessionInner::Postgres(_) => Engine::Postgres,
            SessionInner::Mysql(_) => Engine::Mysql,
            SessionInner::Mssql(_) => Engine::Mssql,
            #[cfg(feature = "duckdb")]
            SessionInner::Duckdb(_) => Engine::Duckdb,
            #[cfg(feature = "mongodb")]
            SessionInner::Mongodb(_) => Engine::Mongodb,
        }
    }

    /// The server this session talks to (MySQL, MariaDB, SQL Server and MongoDB); `None` on
    /// SQLite and DuckDB and, for now, on Postgres.
    pub fn server(&self) -> Option<ServerInfo> {
        match &self.inner {
            SessionInner::Mysql(d) => Some(d.server()),
            SessionInner::Mssql(d) => Some(d.server()),
            SessionInner::Sqlite(_) | SessionInner::Postgres(_) => None,
            #[cfg(feature = "duckdb")]
            SessionInner::Duckdb(_) => None,
            #[cfg(feature = "mongodb")]
            SessionInner::Mongodb(d) => Some(d.server()),
        }
    }

    /// Whether the engine refuses writes in this session.
    pub fn is_read_only(&self) -> bool {
        self.read_only.is_some()
    }

    /// How a read-only session's writes are refused; `None` when the session is not read-only.
    pub fn read_only_enforcement(&self) -> Option<ReadOnlyEnforcement> {
        self.read_only
    }

    /// Whether this session ran a statement that changes session settings (SET or RESET on
    /// Postgres, SET on MySQL, SET, RESET or USE on DuckDB, `use` on MongoDB); text it returns
    /// may then differ from what a fresh session reads back. Always false on SQLite.
    pub fn settings_changed(&self) -> bool {
        match &self.inner {
            SessionInner::Sqlite(_) => false,
            SessionInner::Postgres(d) => d.settings_changed(),
            SessionInner::Mysql(d) => d.settings_changed(),
            SessionInner::Mssql(_) => false,
            #[cfg(feature = "duckdb")]
            SessionInner::Duckdb(d) => d.settings_changed(),
            #[cfg(feature = "mongodb")]
            SessionInner::Mongodb(d) => d.settings_changed(),
        }
    }

    /// What the session's engine supports.
    pub fn capabilities(&self) -> Capabilities {
        self.engine().capabilities()
    }

    /// Starts a script; its statements are prepared one at a time by [`Script::next`].
    pub fn script<'s>(&'s mut self, sql: &str) -> Script<'s> {
        let inner = match &mut self.inner {
            SessionInner::Sqlite(d) => ScriptInner::Sqlite(d.start_script(sql)),
            SessionInner::Postgres(d) => ScriptInner::Postgres(d.start_script(sql)),
            SessionInner::Mysql(d) => ScriptInner::Mysql(d.start_script(sql)),
            SessionInner::Mssql(d) => ScriptInner::Mssql(d.start_script(sql)),
            #[cfg(feature = "duckdb")]
            SessionInner::Duckdb(d) => ScriptInner::Duckdb(d.start_script(sql)),
            #[cfg(feature = "mongodb")]
            SessionInner::Mongodb(d) => ScriptInner::Mongodb(Box::new(d.start_script(sql))),
        };
        Script {
            inner,
            _session: PhantomData,
        }
    }

    /// A handle that cancels this session's running statement from any task or thread.
    pub fn cancel_handle(&self) -> CancelHandle {
        match &self.inner {
            SessionInner::Sqlite(d) => d.cancel_handle(),
            SessionInner::Postgres(d) => d.cancel_handle(),
            SessionInner::Mysql(d) => d.cancel_handle(),
            SessionInner::Mssql(d) => d.cancel_handle(),
            #[cfg(feature = "duckdb")]
            SessionInner::Duckdb(d) => d.cancel_handle(),
            #[cfg(feature = "mongodb")]
            SessionInner::Mongodb(d) => d.cancel_handle(),
        }
    }

    /// Ends any read-only wrapper still open and finishes any pending drain under the 10 s
    /// cap; reports a drain error. Idempotent; a no-op when idle. Past the cap the session
    /// is closed and this returns [`ExecuteError::StoppedAtDrainCap`] for a statement that
    /// may write, or [`ExecuteError::SessionClosed`] as on a session whose connection ended;
    /// MySQL kills any statement with `KILL QUERY` and always returns `StoppedAtDrainCap`.
    /// A caller that runs a second script on a session settles between them, since a stop
    /// kept for a dropped SQL Server script is otherwise answered for the next one.
    pub async fn settle(&mut self) -> Result<(), ExecuteError> {
        match &mut self.inner {
            SessionInner::Sqlite(d) => d.settle().await,
            SessionInner::Postgres(d) => d.settle().await,
            SessionInner::Mysql(d) => d.settle().await,
            SessionInner::Mssql(d) => d.settle().await,
            #[cfg(feature = "duckdb")]
            SessionInner::Duckdb(d) => d.settle().await,
            #[cfg(feature = "mongodb")]
            SessionInner::Mongodb(d) => d.settle().await,
        }
    }

    /// Resolves once the session's connection has ended (the server closed it or the network
    /// dropped); never resolves on SQLite or DuckDB, whose sessions cannot end on their own,
    /// nor on MySQL, SQL Server or MongoDB, whose dropped connection surfaces at the next call.
    /// Cancel-safe.
    pub async fn closed(&self) {
        match &self.inner {
            SessionInner::Sqlite(_) | SessionInner::Mysql(_) | SessionInner::Mssql(_) => {
                std::future::pending().await
            }
            SessionInner::Postgres(d) => d.closed().await,
            #[cfg(feature = "duckdb")]
            SessionInner::Duckdb(_) => std::future::pending().await,
            #[cfg(feature = "mongodb")]
            SessionInner::Mongodb(_) => std::future::pending().await,
        }
    }

    /// Schemas in name order; Postgres leaves out `pg_*` and `information_schema`.
    pub async fn schemas(&mut self) -> Result<Vec<SchemaInfo>, IntrospectError> {
        match &mut self.inner {
            SessionInner::Sqlite(d) => d.schemas().await,
            SessionInner::Postgres(d) => d.schemas().await,
            SessionInner::Mysql(d) => d.schemas().await,
            SessionInner::Mssql(d) => d.schemas().await,
            #[cfg(feature = "duckdb")]
            SessionInner::Duckdb(d) => d.schemas().await,
            #[cfg(feature = "mongodb")]
            SessionInner::Mongodb(d) => d.schemas().await,
        }
    }

    /// Tables and views of `schema` in name order, at most RELATION_CAP; empty for an unknown schema.
    pub async fn relations(&mut self, schema: &str) -> Result<RelationList, IntrospectError> {
        match &mut self.inner {
            SessionInner::Sqlite(d) => d.relations(schema).await,
            SessionInner::Postgres(d) => d.relations(schema).await,
            SessionInner::Mysql(d) => d.relations(schema).await,
            SessionInner::Mssql(d) => d.relations(schema).await,
            #[cfg(feature = "duckdb")]
            SessionInner::Duckdb(d) => d.relations(schema).await,
            #[cfg(feature = "mongodb")]
            SessionInner::Mongodb(d) => d.relations(schema).await,
        }
    }

    /// Columns of `rel` in table order; empty for an unknown relation.
    pub async fn columns(&mut self, rel: &RelationRef) -> Result<Vec<ColumnMeta>, IntrospectError> {
        match &mut self.inner {
            SessionInner::Sqlite(d) => d.columns(rel).await,
            SessionInner::Postgres(d) => d.columns(rel).await,
            SessionInner::Mysql(d) => d.columns(rel).await,
            SessionInner::Mssql(d) => d.columns(rel).await,
            #[cfg(feature = "duckdb")]
            SessionInner::Duckdb(d) => d.columns(rel).await,
            #[cfg(feature = "mongodb")]
            SessionInner::Mongodb(d) => d.columns(rel).await,
        }
    }

    /// Describes `rel` for editing: cast types, writability and row identity.
    /// A relation that does not exist is [`IntrospectError::Missing`].
    pub async fn edit_target(&mut self, rel: &RelationRef) -> Result<EditTarget, IntrospectError> {
        match &mut self.inner {
            SessionInner::Sqlite(d) => sqlite_edit_target(d, rel).await,
            SessionInner::Postgres(d) => pg_edit_target(d, rel).await,
            SessionInner::Mysql(_) | SessionInner::Mssql(_) => Err(IntrospectError::Unsupported {
                what: "edit targets",
            }),
            #[cfg(feature = "duckdb")]
            SessionInner::Duckdb(_) => Err(IntrospectError::Unsupported {
                what: "edit targets",
            }),
            #[cfg(feature = "mongodb")]
            SessionInner::Mongodb(_) => Err(IntrospectError::Unsupported {
                what: "edit targets",
            }),
        }
    }

    /// Keys of `rel` in both directions: outgoing first, then by the far relation, then by
    /// columns. An unknown schema or relation has none.
    pub async fn foreign_keys(
        &mut self,
        rel: &RelationRef,
    ) -> Result<Vec<ForeignKey>, IntrospectError> {
        let mut keys = match &mut self.inner {
            SessionInner::Sqlite(d) => sqlite_foreign_keys(d, rel).await?,
            SessionInner::Postgres(d) => pg_foreign_keys(d, rel).await?,
            SessionInner::Mysql(d) => crate::mysql::catalog::foreign_keys(d, rel).await?,
            SessionInner::Mssql(d) => crate::mssql::catalog::foreign_keys(d, rel).await?,
            #[cfg(feature = "duckdb")]
            SessionInner::Duckdb(_) => {
                return Err(IntrospectError::Unsupported {
                    what: "foreign keys",
                });
            }
            #[cfg(feature = "mongodb")]
            SessionInner::Mongodb(_) => Vec::new(),
        };
        sort_keys(&mut keys);
        Ok(keys)
    }

    /// Keys, indexes, checks and triggers of `rel`; empty lists for an unknown schema or relation,
    /// and empty on DuckDB, whose objects are not read yet.
    pub async fn relation_objects(
        &mut self,
        rel: &RelationRef,
    ) -> Result<RelationObjects, IntrospectError> {
        match &mut self.inner {
            SessionInner::Sqlite(d) => sqlite_relation_objects(d, rel).await,
            SessionInner::Postgres(d) => pg_relation_objects(d, rel).await,
            SessionInner::Mysql(d) => crate::mysql::catalog::relation_objects(d, rel).await,
            SessionInner::Mssql(d) => crate::mssql::catalog::relation_objects(d, rel).await,
            #[cfg(feature = "duckdb")]
            SessionInner::Duckdb(_) => Ok(RelationObjects::new(Vec::new(), Vec::new(), Vec::new())),
            #[cfg(feature = "mongodb")]
            SessionInner::Mongodb(d) => crate::mongo::catalog::relation_objects(d, rel).await,
        }
    }

    /// Types and routines of `schema`, at most RELATION_CAP of each; empty on SQLite, DuckDB and
    /// MongoDB and for an unknown schema.
    pub async fn schema_objects(&mut self, schema: &str) -> Result<SchemaObjects, IntrospectError> {
        match &mut self.inner {
            SessionInner::Sqlite(_) => Ok(SchemaObjects::new(Vec::new(), Vec::new(), false)),
            SessionInner::Postgres(d) => pg_schema_objects(d, schema).await,
            SessionInner::Mysql(d) => crate::mysql::catalog::schema_objects(d, schema).await,
            SessionInner::Mssql(d) => crate::mssql::catalog::schema_objects(d, schema).await,
            #[cfg(feature = "duckdb")]
            SessionInner::Duckdb(_) => Ok(SchemaObjects::new(Vec::new(), Vec::new(), false)),
            #[cfg(feature = "mongodb")]
            SessionInner::Mongodb(_) => Ok(SchemaObjects::new(Vec::new(), Vec::new(), false)),
        }
    }

    /// The current definition of `object`, at most DEFINITION_CAP bytes. SQL Server answers for
    /// views, triggers, routines, checks and synonyms; it stores no text for the rest, which are
    /// Unsupported.
    pub async fn definition(&mut self, object: &ObjectRef) -> Result<Definition, DefinitionError> {
        match &mut self.inner {
            SessionInner::Sqlite(d) => sqlite_definition(d, object).await,
            SessionInner::Postgres(d) => pg_definition(d, object).await,
            SessionInner::Mysql(d) => crate::mysql::catalog::definition(d, object).await,
            SessionInner::Mssql(d) => crate::mssql::catalog::definition(d, object).await,
            #[cfg(feature = "duckdb")]
            SessionInner::Duckdb(d) => crate::duckdb::catalog::definition(d, object).await,
            #[cfg(feature = "mongodb")]
            SessionInner::Mongodb(_) => Err(DefinitionError::Unsupported {
                what: object.label(),
            }),
        }
    }

    /// For each of `columns`, the column of `rel` it reads, or None when it is computed or
    /// reads another table. SQLite and MySQL answer from the columns alone, Postgres with one
    /// catalog query.
    pub async fn source_columns(
        &mut self,
        rel: &RelationRef,
        columns: &[ColumnInfo],
    ) -> Result<Vec<Option<String>>, IntrospectError> {
        match &mut self.inner {
            SessionInner::Sqlite(_) => Ok(crate::sources::named_source_columns(rel, columns)
                .unwrap_or_else(|| vec![None; columns.len()])),
            SessionInner::Postgres(d) => crate::sources::pg_source_columns(d, rel, columns).await,
            SessionInner::Mysql(d) => Ok(crate::mysql::catalog::source_columns(
                rel,
                columns,
                d.fold_names(),
            )),
            SessionInner::Mssql(_) => Ok(vec![None; columns.len()]),
            #[cfg(feature = "duckdb")]
            SessionInner::Duckdb(_) => Ok(vec![None; columns.len()]),
            #[cfg(feature = "mongodb")]
            SessionInner::Mongodb(_) => Ok(vec![None; columns.len()]),
        }
    }

    /// Runs `request` and returns its output. On Postgres every block it opens is rolled back,
    /// also on error, cancel or drop; on SQLite the statement is only planned. A request built
    /// for the other engine is `ExecuteError::Unsupported`.
    pub async fn explain(
        &mut self,
        request: &ExplainRequest,
    ) -> Result<ExplainOutput, ExecuteError> {
        // `sql` is a public field, so a request edited after its check is checked again here:
        // one statement, and that statement an EXPLAIN, or nothing reaches the database.
        if crate::ranges::statement_ranges(request.engine, &request.sql).len() != 1 {
            return Err(ExecuteError::Unsupported {
                reason: "an explain runs exactly one statement".to_owned(),
            });
        }
        if !crate::classify::leading_keyword(&request.sql).eq_ignore_ascii_case("EXPLAIN") {
            return Err(ExecuteError::Unsupported {
                reason: "an explain request must hold an EXPLAIN".to_owned(),
            });
        }
        match (&mut self.inner, request.engine) {
            (SessionInner::Postgres(d), Engine::Postgres) => d
                .explain(&request.sql)
                .await
                .map(ExplainOutput::from_postgres),
            (SessionInner::Sqlite(d), Engine::Sqlite) => {
                let rows = d.explain(&request.sql).await?;
                Ok(ExplainOutput::from_sqlite(&rows))
            }
            _ => Err(ExecuteError::Unsupported {
                reason: "the explain was built for another engine".to_owned(),
            }),
        }
    }

    /// Runs one statement with bound parameters and returns the rows it changed. Used only
    /// by `apply` on its private session, so it bypasses the script and settle machinery.
    pub(crate) async fn run_bound(
        &mut self,
        sql: &str,
        params: &[Param],
    ) -> Result<u64, ExecuteError> {
        match &mut self.inner {
            SessionInner::Sqlite(d) => d.run_bound(sql, params).await,
            SessionInner::Postgres(d) => d.run_bound(sql, params).await,
            SessionInner::Mysql(_) | SessionInner::Mssql(_) => Err(bound_unsupported()),
            #[cfg(feature = "duckdb")]
            SessionInner::Duckdb(_) => Err(bound_unsupported()),
            #[cfg(feature = "mongodb")]
            SessionInner::Mongodb(_) => Err(bound_unsupported()),
        }
    }

    /// Runs a bound statement and returns its rows as a browse reads their values: SQLite
    /// renders numbers as the grid does; on Postgres every result column must be text.
    pub(crate) async fn query_bound(
        &mut self,
        sql: &str,
        params: &[Param],
    ) -> Result<Vec<Vec<EditValue>>, ExecuteError> {
        match &mut self.inner {
            SessionInner::Sqlite(d) => d.query_bound(sql, params).await,
            SessionInner::Postgres(d) => Ok(d
                .query_bound(sql, params)
                .await?
                .into_iter()
                .map(|row| {
                    row.into_iter()
                        .map(|v| v.map_or(EditValue::Null, EditValue::Text))
                        .collect()
                })
                .collect()),
            SessionInner::Mysql(_) | SessionInner::Mssql(_) => Err(bound_unsupported()),
            #[cfg(feature = "duckdb")]
            SessionInner::Duckdb(_) => Err(bound_unsupported()),
            #[cfg(feature = "mongodb")]
            SessionInner::Mongodb(_) => Err(bound_unsupported()),
        }
    }

    /// Closes the connection and waits for the engine to let go of it. A truncated write's
    /// unread rows are drained first, so its commit is kept; an error from that drain, or one
    /// an earlier drain left unreported, is returned. Past the drain cap Postgres stops the
    /// write and returns [`ExecuteError::StoppedAtDrainCap`]; MySQL kills any statement with
    /// `KILL QUERY` and returns the same; SQLite returns [`ExecuteError::SessionClosed`] when
    /// its write does not stop, since its fate is unknown.
    pub async fn close(self) -> Result<(), ExecuteError> {
        match self.inner {
            SessionInner::Sqlite(d) => d.close().await,
            SessionInner::Postgres(d) => d.close().await,
            SessionInner::Mysql(d) => d.close().await,
            SessionInner::Mssql(d) => d.close().await,
            #[cfg(feature = "duckdb")]
            SessionInner::Duckdb(d) => d.close().await,
            #[cfg(feature = "mongodb")]
            SessionInner::Mongodb(d) => d.close().await,
        }
    }
}

/// The statements of one script, in order, each prepared only when asked for.
pub struct Script<'s> {
    inner: ScriptInner,
    _session: PhantomData<&'s mut Session>,
}

impl Script<'_> {
    /// Prepares the next statement; `None` once the script is done. A statement that fails
    /// to prepare ends the script.
    pub async fn next(&mut self) -> Option<Result<Pending<'_>, ExecuteError>> {
        let prepared = match &mut self.inner {
            ScriptInner::Sqlite(s) => SqliteDriver::prepare_next(s).await?,
            ScriptInner::Postgres(s) => PgDriver::prepare_next(s).await?,
            ScriptInner::Mysql(s) => MysqlDriver::prepare_next(s).await?,
            ScriptInner::Mssql(s) => MssqlDriver::prepare_next(s).await?,
            #[cfg(feature = "duckdb")]
            ScriptInner::Duckdb(s) => DuckDriver::prepare_next(s).await?,
            #[cfg(feature = "mongodb")]
            ScriptInner::Mongodb(s) => MongoDriver::prepare_next(s).await?,
        };
        Some(prepared.map(|prepared| Pending {
            script: &mut self.inner,
            prepared,
        }))
    }
}

/// A prepared statement, waiting to be executed or skipped.
pub struct Pending<'a> {
    script: &'a mut ScriptInner,
    prepared: Prepared,
}

impl<'a> Pending<'a> {
    /// The statement's exact text.
    pub fn sql(&self) -> &str {
        &self.prepared.sql
    }

    /// What the statement does, known before it runs.
    pub fn classification(&self) -> &Classification {
        &self.prepared.classification
    }

    /// Whether no statement follows this one in the script. Nothing runs to find out; a
    /// following statement that cannot be prepared yet still counts as following.
    pub async fn is_last(&mut self) -> bool {
        match &mut *self.script {
            ScriptInner::Sqlite(s) => SqliteDriver::is_last(s).await,
            ScriptInner::Postgres(s) => PgDriver::is_last(s).await,
            ScriptInner::Mysql(s) => MysqlDriver::is_last(s).await,
            ScriptInner::Mssql(s) => MssqlDriver::is_last(s).await,
            #[cfg(feature = "duckdb")]
            ScriptInner::Duckdb(s) => DuckDriver::is_last(s).await,
            #[cfg(feature = "mongodb")]
            ScriptInner::Mongodb(s) => MongoDriver::is_last(s).await,
        }
    }

    /// Runs the statement. An error on its first step comes back here; later errors come
    /// from [`ResultStream::next_batch`].
    pub async fn execute(self, limits: Limits) -> Result<ResultStream<'a>, ExecuteError> {
        let (columns, stream) = match &mut *self.script {
            ScriptInner::Sqlite(s) => {
                let (columns, stream) = SqliteDriver::execute(s, limits).await?;
                (columns, StreamInner::Sqlite(stream))
            }
            ScriptInner::Postgres(s) => {
                let (columns, stream) = PgDriver::execute(s, limits).await?;
                (columns, StreamInner::Postgres(stream))
            }
            ScriptInner::Mysql(s) => {
                let (columns, stream) = MysqlDriver::execute(s, limits).await?;
                (columns, StreamInner::Mysql(stream))
            }
            ScriptInner::Mssql(s) => {
                let (columns, stream) = MssqlDriver::execute(s, limits).await?;
                (columns, StreamInner::Mssql(stream))
            }
            #[cfg(feature = "duckdb")]
            ScriptInner::Duckdb(s) => {
                let (columns, stream) = DuckDriver::execute(s, limits).await?;
                (columns, StreamInner::Duckdb(stream))
            }
            #[cfg(feature = "mongodb")]
            // Pinned on the heap: its future is several times the size of any SQL engine's, and
            // inline it would set the size of every engine's `execute`.
            ScriptInner::Mongodb(s) => {
                let (columns, stream) = Box::pin(MongoDriver::execute(s, limits)).await?;
                (columns, StreamInner::Mongodb(Box::new(stream)))
            }
        };
        Ok(ResultStream {
            _script: self.script,
            stream,
            columns,
            outcome: None,
            failed: false,
        })
    }

    /// Leaves the statement unrun; the script moves on at the next [`Script::next`].
    pub fn skip(self) {}
}

/// The rows of a running statement, delivered in batches.
pub struct ResultStream<'a> {
    _script: &'a mut ScriptInner,
    stream: StreamInner,
    columns: Vec<ColumnInfo>,
    outcome: Option<Outcome>,
    /// Set once `next_batch` has failed; the stream then reads as ended.
    failed: bool,
}

impl ResultStream<'_> {
    /// The result columns; empty for a statement without rows.
    pub fn columns(&self) -> &[ColumnInfo] {
        &self.columns
    }

    /// The next batch of rows, or `None` once the statement is done.
    pub async fn next_batch(&mut self) -> Result<Option<ResultBatch>, ExecuteError> {
        if self.outcome.is_some() || self.failed {
            return Ok(None);
        }
        let item = match &mut self.stream {
            StreamInner::Sqlite(s) => SqliteDriver::next_event(s).await,
            StreamInner::Postgres(s) => PgDriver::next_event(s).await,
            StreamInner::Mysql(s) => MysqlDriver::next_event(s).await,
            StreamInner::Mssql(s) => MssqlDriver::next_event(s).await,
            #[cfg(feature = "duckdb")]
            StreamInner::Duckdb(s) => DuckDriver::next_event(s).await,
            #[cfg(feature = "mongodb")]
            StreamInner::Mongodb(s) => MongoDriver::next_event(s).await,
        };
        let item = match item {
            Ok(i) => i,
            Err(e) => {
                self.failed = true;
                return Err(e);
            }
        };
        match item {
            StreamItem::Batch(b) => Ok(Some(b)),
            StreamItem::Done(o) => {
                self.outcome = Some(o);
                Ok(None)
            }
        }
    }

    /// How the statement ended; `Some` once [`next_batch`](Self::next_batch) returned `None`.
    pub fn outcome(&self) -> Option<&Outcome> {
        self.outcome.as_ref()
    }

    /// Resumes a stream parked at its row cap. `more` raises `max_rows` by that many rows;
    /// `None` removes the row cap. `max_bytes` still applies. Errors when the stream is not
    /// parked; it does no I/O, so a connection that died while parked surfaces from the next
    /// `next_batch` instead.
    pub async fn extend(&mut self, more: Option<u64>) -> Result<(), ExtendError> {
        let at_row_cut = self
            .outcome
            .as_ref()
            .is_some_and(|o| o.truncated_by == Some(TruncatedBy::Rows));
        if self.failed || !at_row_cut {
            return Err(ExtendError::NotParked);
        }
        match &mut self.stream {
            StreamInner::Sqlite(s) => SqliteDriver::extend(s, more)?,
            StreamInner::Postgres(s) => PgDriver::extend(s, more)?,
            StreamInner::Mysql(s) => MysqlDriver::extend(s, more)?,
            StreamInner::Mssql(s) => MssqlDriver::extend(s, more)?,
            #[cfg(feature = "duckdb")]
            StreamInner::Duckdb(s) => DuckDriver::extend(s, more)?,
            #[cfg(feature = "mongodb")]
            StreamInner::Mongodb(s) => MongoDriver::extend(s, more)?,
        }
        self.outcome = None;
        Ok(())
    }
}

/// Stops the session's running statement from any thread. Settle spends earlier cancels; one
/// issued after a statement's settle stops that statement before it is sent. The fallback
/// cancel paths and a cancel slower than the settle's wait are not fenced this way.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct CancelHandle {
    inner: CancelInner,
}

#[derive(Debug, Clone)]
enum CancelInner {
    Sqlite(Arc<SqliteCancel>),
    Postgres(Arc<PgCancel>),
    Mysql(Arc<MysqlCancel>),
    Mssql(Arc<MssqlCancel>),
    #[cfg(feature = "duckdb")]
    Duckdb(Arc<DuckCancel>),
    #[cfg(feature = "mongodb")]
    Mongodb(Arc<MongoCancel>),
}

impl CancelHandle {
    pub(crate) fn sqlite(cancel: Arc<SqliteCancel>) -> Self {
        CancelHandle {
            inner: CancelInner::Sqlite(cancel),
        }
    }

    pub(crate) fn postgres(cancel: Arc<PgCancel>) -> Self {
        CancelHandle {
            inner: CancelInner::Postgres(cancel),
        }
    }

    pub(crate) fn mysql(cancel: Arc<MysqlCancel>) -> Self {
        CancelHandle {
            inner: CancelInner::Mysql(cancel),
        }
    }

    pub(crate) fn mssql(cancel: Arc<MssqlCancel>) -> Self {
        CancelHandle {
            inner: CancelInner::Mssql(cancel),
        }
    }

    #[cfg(feature = "duckdb")]
    pub(crate) fn duckdb(cancel: Arc<DuckCancel>) -> Self {
        CancelHandle {
            inner: CancelInner::Duckdb(cancel),
        }
    }

    #[cfg(feature = "mongodb")]
    pub(crate) fn mongo(cancel: Arc<MongoCancel>) -> Self {
        CancelHandle {
            inner: CancelInner::Mongodb(cancel),
        }
    }

    /// Best effort; never blocks. A Postgres cancel runs on the runtime that opened the
    /// session, so once that runtime has shut down this does nothing.
    pub fn cancel(&self) {
        match &self.inner {
            CancelInner::Sqlite(c) => c.cancel(),
            CancelInner::Postgres(c) => c.cancel(),
            CancelInner::Mysql(c) => c.cancel(),
            CancelInner::Mssql(c) => c.cancel(),
            #[cfg(feature = "duckdb")]
            CancelInner::Duckdb(c) => c.cancel(),
            #[cfg(feature = "mongodb")]
            CancelInner::Mongodb(c) => c.cancel(),
        }
    }
}

/// Bound statements back row edits, which MySQL, SQL Server, DuckDB and MongoDB sessions do not
/// support yet.
fn bound_unsupported() -> ExecuteError {
    ExecuteError::Unsupported {
        reason: "bound statements are not built for this engine yet".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_mysql_says_a_non_transactional_write_may_be_kept() {
        let e = ExecuteError::StoppedAtDrainCap;
        assert_eq!(e.message_for(Engine::Postgres), e.to_string());
        assert!(
            e.message_for(Engine::Mysql)
                .ends_with("may have kept part of its changes")
        );
        assert_eq!(
            ExecuteError::Timeout.message_for(Engine::Mysql),
            "the statement timed out"
        );
    }

    #[test]
    fn mongodb_says_a_stopped_statement_may_have_written() {
        assert_eq!(
            ExecuteError::Timeout.message_for(Engine::Mongodb),
            "the statement timed out; a write may have kept part of its changes"
        );
        assert_eq!(
            ExecuteError::Cancelled.message_for(Engine::Mongodb),
            "the statement was cancelled here; a write may keep running on the server until it \
             ends or hits the time limit"
        );
    }

    #[test]
    fn the_sql_server_drain_cap_text_names_any_cut_short_or_abandoned_result() {
        let text = ExecuteError::StoppedAtDrainCap.message_for(Engine::Mssql);
        assert!(
            text.contains("cut short or abandoned") && !text.contains("row limit"),
            "{text}"
        );
        assert!(text.contains("keep what they wrote"), "{text}");
    }
}
