//! What every tool call shares: the session it opened, cancellation and the history.

use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Duration;

use dbento_core::{
    CancelHandle, ConnectionConfig, Engine, ExecuteError, History, HistoryEntry,
    ReadOnlyEnforcement, Session, read_only_residual,
};
use rmcp::RoleServer;
use rmcp::service::RequestContext;

use crate::error::ToolError;

const ENFORCED: &str = "Read-only sessions on this connection are enforced by the database.";

// What the m1 spec records as outside `BEGIN READ ONLY`.
const POSTGRES: &str = "Read-only sessions on this connection are enforced by the database: each \
    statement runs in `BEGIN READ ONLY`. The server still lets a few things through: functions \
    that open their own connections, such as `dblink_exec` where the dblink extension is \
    installed, and admin functions whose effect is not transactional, such as \
    `pg_terminate_backend`, `pg_cancel_backend` and `pg_stat_reset`. For a production database, \
    use a role without the privileges the rest need; it can still cancel or terminate backends of \
    its own role.";

// MariaDB is the same engine and takes the MYSQL text; the holes it lists are the README's.
const MYSQL: &str = "Read-only sessions on this connection are enforced by the database: each \
    statement runs in `START TRANSACTION READ ONLY`. The server still lets a few things through: \
    `SELECT ... INTO OUTFILE` or `DUMPFILE`, `SET GLOBAL` and `SET PERSIST`, `FLUSH`, `GET_LOCK`, \
    and an existing stored procedure that commits and then writes. For a production database, \
    use an account without the FILE, RELOAD, SYSTEM_VARIABLES_ADMIN or SUPER privileges and \
    without EXECUTE on such procedures, ideally one granted only SELECT.";

// The claims below are measured, and the forms BEST_EFFORT quotes are pinned in
// classify/mssql/tests.rs; a claim added here needs a measurement first.
const SQL_SERVER_READER: &str = "Read-only sessions on this connection are enforced by the \
    database: they run as the `read_only_user` principal, so permission checks refuse writes. \
    Not closed: procedures granted to public that write through ownership chaining, here or in \
    master and msdb (`msdb.dbo.sp_sysdac_delete_history` runs for any login), and the principal's \
    own DEFAULT_SCHEMA unless the setup denies ALTER ANY USER. A hidden commit followed by an \
    error keeps such a write and reports only the error.";

const BEST_EFFORT: &str = "Read-only sessions on this connection are best effort, not enforced \
    by the database: without `read_only_user`, Dbento fences each batch in a transaction it \
    rolls back and refuses the batches it can parse as writes, a lone EXEC included. A login \
    with the rights can get past the fence with a hidden commit in a batch the parser rejects: \
    `SELECT 1 AS one EXEC('COMMIT COMMIT DROP DATABASE victim')` drops the database, \
    `SELECT 1 AS one EXEC('COMMIT COMMIT KILL 999')` reaches KILL, and \
    `EXEC('COMMIT COMMIT') SHUTDOWN WITH NOWAIT` stops the server, with SHUTDOWN as a top-level \
    statement (the server refuses it inside an EXEC string). Ordinary writes in such a batch \
    persist too, and Dbento does not always report them: not when the batch then raises an \
    error or is stopped, and not when the commit sits in the part of a response a cut discards. \
    Some effects need no commit: UPDATE STATISTICS, sequence values and identity values move \
    although the fence rolls back, as do server-level side effects the login's privileges allow. \
    By decision SHUTDOWN stays out of Dbento's refusals (layer 3): `read_only_user` is the \
    answer. The session then runs as a reader principal, which holds none of those rights.";

// The level follows the login, so the text names both and leaves the answer to each `query`;
// the residual is core's, shared with the CLI and the app.
static SURREALDB: LazyLock<String> = LazyLock::new(|| {
    format!(
        "The level of a read-only session on this connection depends on its login and is known \
         only after connecting: `database` for a VIEWER login, whose writes the server refuses, \
         and `best_effort` for any other, where Dbento runs each statement in a transaction it \
         cancels and refuses statements it reads as writes. Each `query` result carries the \
         level. {}",
        read_only_residual(Engine::Surrealdb).unwrap_or_default()
    )
});

const UNKNOWN: &str = "The enforcement of read-only sessions on this connection is unknown to \
    this server.";

/// The level's name in `list_connections`.
pub(crate) fn enforcement_name(level: ReadOnlyEnforcement) -> &'static str {
    match level {
        ReadOnlyEnforcement::Database => "database",
        ReadOnlyEnforcement::BestEffort => "best_effort",
        _ => "unknown",
    }
}

/// What a read-only session at `level` on `engine` does and does not stop, in words for a client.
fn session_description(engine: Engine, level: ReadOnlyEnforcement) -> &'static str {
    match level {
        ReadOnlyEnforcement::Database if engine == Engine::Mssql => SQL_SERVER_READER,
        ReadOnlyEnforcement::Database if engine == Engine::Mysql => MYSQL,
        ReadOnlyEnforcement::Database if engine == Engine::Postgres => POSTGRES,
        ReadOnlyEnforcement::Database => ENFORCED,
        ReadOnlyEnforcement::BestEffort => BEST_EFFORT,
        _ => UNKNOWN,
    }
}

/// The level name and text `list_connections` shows for a connection configured at `level`.
/// SurrealDB's level follows the login, which only a connection reveals.
pub(crate) fn listed_level(
    engine: Engine,
    level: ReadOnlyEnforcement,
) -> (&'static str, &'static str) {
    if engine == Engine::Surrealdb {
        return ("unknown", SURREALDB.as_str());
    }
    (enforcement_name(level), session_description(engine, level))
}

/// The server's timeout or the connection's own, whichever is shorter.
pub(crate) fn effective_timeout(flag: Duration, own: Option<Duration>) -> Duration {
    own.map_or(flag, |own| own.min(flag))
}

/// What a client's cancel answers on `engine`, worded for what the stop kept.
fn cancelled(engine: Engine) -> ToolError {
    ToolError::from_execute_on(&ExecuteError::Cancelled, engine)
}

/// Runs `work` until it ends or the client cancels the request; a cancel fires `cancel` and
/// answers `cancelled` in `engine`'s words.
pub(crate) async fn until_cancelled<T>(
    context: &RequestContext<RoleServer>,
    cancel: &CancelHandle,
    engine: Engine,
    work: impl Future<Output = Result<T, ToolError>>,
) -> Result<T, ToolError> {
    // rmcp keeps the handler running after a client cancel, so the statement is stopped in
    // the database here; dropping the work alone would leave it running server side.
    tokio::select! {
        r = work => r,
        () = context.ct.cancelled() => {
            cancel.cancel();
            Err(cancelled(engine))
        }
    }
}

/// A session on one exposed connection, with where it came from.
pub(crate) struct Opened {
    /// The workspace folder the connection was read from.
    pub dir: PathBuf,
    /// The connection as configured when the call started.
    pub conn: ConnectionConfig,
    /// The open session; the caller closes it.
    pub session: Session,
}

/// Appends `entry` off the async runtime; a failure goes to stderr, never to the client.
pub(crate) async fn record(path: &Path, entry: HistoryEntry) {
    let path = path.to_path_buf();
    let written = tokio::task::spawn_blocking(move || History::open(&path)?.append(&entry))
        .await
        .map_err(|e| e.to_string())
        .and_then(|r| r.map_err(|e| e.to_string()));
    if let Err(e) = written {
        eprintln!("dbento mcp: cannot write the query history: {e}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_shorter_timeout_wins() {
        let s = Duration::from_secs;
        assert_eq!(effective_timeout(s(30), None), s(30));
        assert_eq!(
            effective_timeout(s(30), Some(Duration::from_millis(300))),
            Duration::from_millis(300)
        );
        assert_eq!(effective_timeout(s(1), Some(s(10))), s(1));
    }

    #[test]
    fn a_client_cancel_on_sql_server_says_what_the_batch_may_have_kept() {
        let on_mssql = cancelled(Engine::Mssql);
        assert_eq!(on_mssql.code(), "cancelled");
        assert!(
            on_mssql.message().contains("may have kept"),
            "{}",
            on_mssql.message()
        );
        assert_eq!(
            cancelled(Engine::Postgres).message(),
            "the statement was cancelled"
        );
    }

    // The exact text is the contract: the standing decision says what a client must be told.
    #[test]
    fn a_best_effort_session_states_the_hole_with_its_true_forms_and_the_fix() {
        assert_eq!(
            session_description(Engine::Mssql, ReadOnlyEnforcement::BestEffort),
            "Read-only sessions on this connection are best effort, not enforced by the database: \
             without `read_only_user`, Dbento fences each batch in a transaction it rolls back and \
             refuses the batches it can parse as writes, a lone EXEC included. A login with the \
             rights can get past the fence with a hidden commit in a batch the parser rejects: \
             `SELECT 1 AS one EXEC('COMMIT COMMIT DROP DATABASE victim')` drops the database, \
             `SELECT 1 AS one EXEC('COMMIT COMMIT KILL 999')` reaches KILL, and \
             `EXEC('COMMIT COMMIT') SHUTDOWN WITH NOWAIT` stops the server, with SHUTDOWN as a \
             top-level statement (the server refuses it inside an EXEC string). Ordinary writes in \
             such a batch persist too, and Dbento does not always report them: not when the batch \
             then raises an error or is stopped, and not when the commit sits in the part of a \
             response a cut discards. Some effects need no commit: UPDATE STATISTICS, sequence \
             values and identity values move although the fence rolls back, as do server-level \
             side effects the login's privileges allow. By decision SHUTDOWN stays out of \
             Dbento's refusals (layer \
             3): `read_only_user` is the answer. The session then runs as a reader principal, \
             which holds none of those rights."
        );
    }

    #[test]
    fn a_postgres_session_lists_what_the_server_still_lets_through() {
        assert_eq!(
            session_description(Engine::Postgres, ReadOnlyEnforcement::Database),
            "Read-only sessions on this connection are enforced by the database: each statement \
             runs in `BEGIN READ ONLY`. The server still lets a few things through: functions \
             that open their own connections, such as `dblink_exec` where the dblink extension \
             is installed, and admin functions whose effect is not transactional, such as \
             `pg_terminate_backend`, \
             `pg_cancel_backend` and `pg_stat_reset`. For a production database, use a role \
             without the privileges the rest need; it can still cancel or terminate backends of \
             its own role."
        );
    }

    // MariaDB shares this arm: `mariadb` is an alias of the `mysql` engine.
    #[test]
    fn a_mysql_session_lists_what_the_server_still_lets_through() {
        assert_eq!(
            session_description(Engine::Mysql, ReadOnlyEnforcement::Database),
            "Read-only sessions on this connection are enforced by the database: each statement \
             runs in `START TRANSACTION READ ONLY`. The server still lets a few things through: \
             `SELECT ... INTO OUTFILE` or `DUMPFILE`, `SET GLOBAL` and `SET PERSIST`, `FLUSH`, \
             `GET_LOCK`, and an existing stored procedure that commits and then writes. For a \
             production database, use an account without the FILE, RELOAD, \
             SYSTEM_VARIABLES_ADMIN or SUPER privileges and without EXECUTE on such procedures, \
             ideally one granted only SELECT."
        );
    }

    #[test]
    fn a_database_session_says_who_enforces_it_and_a_reader_its_documented_limit() {
        assert_eq!(
            session_description(Engine::Sqlite, ReadOnlyEnforcement::Database),
            "Read-only sessions on this connection are enforced by the database."
        );
        assert_eq!(
            session_description(Engine::Mssql, ReadOnlyEnforcement::Database),
            "Read-only sessions on this connection are enforced by the database: they run as the \
             `read_only_user` principal, so permission checks refuse writes. Not closed: procedures \
             granted to public that write through ownership chaining, here or in master and msdb \
             (`msdb.dbo.sp_sysdac_delete_history` runs for any login), and the principal's own \
             DEFAULT_SCHEMA unless the setup denies ALTER ANY USER. A hidden commit followed by an \
             error keeps such a write and reports only the error."
        );
        assert_eq!(enforcement_name(ReadOnlyEnforcement::Database), "database");
        assert_eq!(
            enforcement_name(ReadOnlyEnforcement::BestEffort),
            "best_effort"
        );
    }
}
