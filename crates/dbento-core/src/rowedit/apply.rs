//! Applying a batch: every change runs in one transaction on a private session, and the
//! caller sees each change's outcome before choosing to commit or roll back.

use std::fmt;

use super::sql::{Param, bound};
use super::{EditBatch, EditValue, RowKey};
use crate::Secret;
use crate::introspect::quote_literal;
use crate::session::{ConnectError, ExecuteError, Session, SessionOptions, connect};
use crate::workspace::{ConnectionConfig, Engine, Workspace};

/// What one change did inside the apply transaction.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeOutcome {
    /// The change touched this many rows (one, for a batch that can commit).
    Applied(u64),
    /// The row was not found or no longer holds the loaded values: someone else changed or
    /// removed it.
    Conflict,
    /// The key matched this many rows, more than one.
    Ambiguous(u64),
    /// The database refused the change with this message.
    Failed(String),
    /// Not sent, because an earlier change failed.
    NotAttempted,
}

/// Why a batch could not be applied.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum ApplyError {
    /// The connection is read-only; nothing was sent.
    #[error("the connection is read-only")]
    ReadOnly,
    /// The private session could not be opened.
    #[error(transparent)]
    Connect(#[from] ConnectError),
    /// BEGIN, ROLLBACK or a session setting failed.
    #[error("transaction control failed: {0}")]
    Session(ExecuteError),
    /// The database refused COMMIT, so nothing was committed.
    #[error("the commit failed, so nothing was committed: {0}")]
    CommitFailed(ExecuteError),
    /// COMMIT got no answer from the database (the connection ended, or the commit timed
    /// out or was cancelled), so whether the changes were saved is unknown.
    #[error("the commit got no answer, so whether the changes were saved is unknown: {0}")]
    CommitUnknown(ExecuteError),
    /// Not every change touched exactly one row; nothing was committed.
    #[error("the changes were rolled back")]
    Rejected {
        /// One outcome per change, in batch order.
        outcomes: Vec<ChangeOutcome>,
    },
}

/// A batch applied in an open transaction, waiting for commit or rollback. Dropping it
/// drops the connection, so the server rolls the transaction back.
#[must_use]
#[non_exhaustive]
pub struct Applied {
    session: Session,
    affected: u64,
    /// Per change, the identity the database wrote when the change set a key column.
    keys: Vec<Option<RowKey>>,
}

impl fmt::Debug for Applied {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Applied")
            .field("affected", &self.affected)
            .finish_non_exhaustive()
    }
}

impl Applied {
    /// The rows the batch touched.
    pub fn affected(&self) -> u64 {
        self.affected
    }

    /// The identity change `change` left its row with, as the database stored it and a
    /// browse reads it; None unless that change is an Update that set a key column or
    /// moved a row its ctid locates, and None on a table whose rules refuse `RETURNING`.
    pub fn written_key(&self, change: usize) -> Option<&RowKey> {
        self.keys.get(change)?.as_ref()
    }

    /// Commits the transaction, then closes the session. A refusal by the database is
    /// [`ApplyError::CommitFailed`]; any other failure is [`ApplyError::CommitUnknown`].
    pub async fn commit(self) -> Result<(), ApplyError> {
        self.finish("COMMIT").await.map_err(|e| match e {
            ExecuteError::Database { .. } => ApplyError::CommitFailed(e),
            other @ (ExecuteError::Timeout
            | ExecuteError::Cancelled
            | ExecuteError::ReadOnlyViolation { .. }
            | ExecuteError::Unsupported { .. }
            | ExecuteError::TransactionControl { .. }
            | ExecuteError::SessionClosed
            | ExecuteError::StoppedAtDrainCap
            | ExecuteError::Syntax { .. }
            | ExecuteError::ResultExpired
            | ExecuteError::SessionEnded { .. }) => ApplyError::CommitUnknown(other),
        })
    }

    /// Rolls the transaction back, then closes the session.
    pub async fn rollback(self) -> Result<(), ApplyError> {
        self.finish("ROLLBACK").await.map_err(ApplyError::Session)
    }

    async fn finish(mut self, sql: &str) -> Result<(), ExecuteError> {
        let result = self.session.run_bound(sql, &[]).await;
        // `run_bound` bypasses scripts, so close has no drain to report.
        let _ = self.session.close().await;
        result.map(drop)
    }
}

/// Runs `batch` in one transaction on a new session of `conn`. Returns [`Applied`] with the
/// transaction still open when every change touched exactly one row; otherwise rolls back
/// and returns [`ApplyError::Rejected`] with each change's outcome.
pub async fn apply(
    ws: &Workspace,
    conn: &ConnectionConfig,
    secret: Option<Secret>,
    batch: EditBatch,
) -> Result<Applied, ApplyError> {
    apply_with(ws, conn, secret, SessionOptions::default(), batch).await
}

/// As [`apply`], opening the session with `options` (such as a client key's passphrase).
pub async fn apply_with(
    ws: &Workspace,
    conn: &ConnectionConfig,
    secret: Option<Secret>,
    options: SessionOptions,
    batch: EditBatch,
) -> Result<Applied, ApplyError> {
    if conn.read_only() {
        return Err(ApplyError::ReadOnly);
    }
    let mut session = connect(ws, conn, secret, options).await?;
    let mut started = pin_session(&mut session, conn.engine(), batch.target().pins()).await;
    if started.is_ok() {
        // MySQL reads BEGIN as a block opener in stored programs; START TRANSACTION never is.
        // SQLite takes the write lock up front, so no other writer can slip in between a
        // guard's read and its write. Postgres reads committed rows whatever the database
        // default, so a guard that waited on a row lock sees the row its holder committed.
        let begin = match conn.engine() {
            Engine::Mysql => "START TRANSACTION",
            Engine::Sqlite => "BEGIN IMMEDIATE",
            Engine::Postgres => "BEGIN ISOLATION LEVEL READ COMMITTED",
            Engine::Mssql
            | Engine::Duckdb
            | Engine::Mongodb
            | Engine::Surrealdb
            | Engine::Redis
            | Engine::Dynamodb
            | Engine::S3 => "BEGIN",
        };
        started = session.run_bound(begin, &[]).await.map(drop);
    }
    if let Err(e) = started {
        let _ = session.close().await;
        // SQLite's BEGIN is the only statement before the changes, and the database refuses
        // it when another writer holds the lock: that fails the first change, as a write would.
        if conn.engine() == Engine::Sqlite
            && let ExecuteError::Database { message, .. } = e
        {
            let outcomes = std::iter::once(ChangeOutcome::Failed(message))
                .chain(std::iter::repeat_n(
                    ChangeOutcome::NotAttempted,
                    batch.changes().len().saturating_sub(1),
                ))
                .collect();
            return Err(ApplyError::Rejected { outcomes });
        }
        return Err(ApplyError::Session(e));
    }
    // Conflicts and ambiguities leave the transaction usable, so the run continues to
    // report every row; a failure aborts it, so later changes are not sent.
    let mut outcomes = Vec::with_capacity(batch.changes().len());
    let mut keys = Vec::with_capacity(batch.changes().len());
    let mut failed = false;
    let identity = batch.target().identity();
    let as_key =
        |values: Vec<EditValue>| RowKey::new(identity.iter().cloned().zip(values).collect());
    for statement in bound(&batch) {
        if failed {
            outcomes.push(ChangeOutcome::NotAttempted);
            continue;
        }
        // The guard reads the row first; a row that no longer holds the loaded values is
        // reported without sending the write, and the run goes on to the next change.
        if let Some(guard) = statement.guard.as_deref() {
            let checked = match session.query_bound(&guard.sql, &guard.params).await {
                Ok(rows) => guard_outcome(&rows, &statement.expected),
                Err(e) => Some(ChangeOutcome::Failed(failure_message(e))),
            };
            if let Some(outcome) = checked {
                failed = matches!(outcome, ChangeOutcome::Failed(_));
                outcomes.push(outcome);
                keys.push(None);
                continue;
            }
        }
        // A write with expected values but no guard returns the row it changed, which must
        // still hold them; a mismatch is reported and the batch rolls the write back.
        if statement.guard.is_none() && !statement.expected.is_empty() {
            let outcome = match session.query_bound(&statement.sql, &statement.params).await {
                Ok(rows) => {
                    guard_outcome(&rows, &statement.expected).unwrap_or(ChangeOutcome::Applied(1))
                }
                Err(e) => ChangeOutcome::Failed(failure_message(e)),
            };
            failed = matches!(outcome, ChangeOutcome::Failed(_));
            outcomes.push(outcome);
            keys.push(None);
            continue;
        }
        // A statement that returns the identity counts its rows; the one row is the key.
        let ran = if statement.returning {
            session
                .query_bound(&statement.sql, &statement.params)
                .await
                .map(|rows| (rows.len() as u64, rows.into_iter().next()))
        } else {
            session
                .run_bound(&statement.sql, &statement.params)
                .await
                .map(|n| (n, None))
        };
        // The warning check runs straight after the change, before a re-read replaces the
        // server's warning list; only a clean single-row change learns its written key.
        let mut key = None;
        let outcome = match ran {
            Err(e) => ChangeOutcome::Failed(failure_message(e)),
            Ok((matched, row)) => {
                let outcome = match session.bound_warnings().await {
                    Err(e) => ChangeOutcome::Failed(failure_message(e)),
                    Ok(warnings) => change_outcome(matched, &warnings),
                };
                if outcome == ChangeOutcome::Applied(1) {
                    key = row.map(as_key);
                }
                outcome
            }
        };
        let outcome = match (outcome, statement.reread.as_deref()) {
            (ChangeOutcome::Applied(1), Some(reread)) => {
                match session.query_bound(&reread.sql, &reread.params).await {
                    Ok(mut rows) => {
                        if rows.len() == 1 {
                            key = rows.pop().map(as_key);
                        }
                        ChangeOutcome::Applied(1)
                    }
                    Err(e) => ChangeOutcome::Failed(failure_message(e)),
                }
            }
            (other, _) => other,
        };
        failed = matches!(outcome, ChangeOutcome::Failed(_));
        outcomes.push(outcome);
        keys.push(key);
    }
    if outcomes.iter().all(|o| *o == ChangeOutcome::Applied(1)) {
        let affected = outcomes.len() as u64;
        return Ok(Applied {
            session,
            affected,
            keys,
        });
    }
    // A lost connection rolls back on its own, so a failed ROLLBACK changes nothing.
    let _ = session.run_bound("ROLLBACK", &[]).await;
    let _ = session.close().await;
    Err(ApplyError::Rejected { outcomes })
}

/// Sets everything an apply session needs before BEGIN: the lock timeouts and the target's
/// pins, the browse session's settings that shape the text of keys and values.
pub(super) async fn pin_session(
    session: &mut Session,
    engine: Engine,
    pins: &[(String, String)],
) -> Result<(), ExecuteError> {
    // On Postgres a row locked elsewhere fails the change after five seconds instead of
    // hanging; the session is private to this apply, so a session-level SET is enough.
    // SQLite waits on the 5000 ms busy timeout its connection already has.
    if engine == Engine::Postgres {
        session.run_bound("SET lock_timeout = '5s'", &[]).await?;
    }
    // MySQL waits five seconds on row and metadata locks alike, and takes the browse
    // session's time zone and SQL mode over the text protocol, the mode made strict so a
    // value too long or out of range errors instead of being silently cut.
    if engine == Engine::Mysql {
        session
            .run_bound(
                "SET SESSION innodb_lock_wait_timeout = 5, lock_wait_timeout = 5",
                &[],
            )
            .await?;
        if !pins.is_empty() {
            let sets = pins
                .iter()
                .map(|(name, value)| {
                    let value = if name == "sql_mode" {
                        strict_sql_mode(value)
                    } else {
                        value.clone()
                    };
                    format!("{name} = {}", quote_literal(Engine::Mysql, &value))
                })
                .collect::<Vec<_>>()
                .join(", ");
            session
                .run_bound(&format!("SET SESSION {sets}"), &[])
                .await?;
        }
    }
    // Key text is the browse session's output text, so the apply session takes on that
    // session's DateStyle, TimeZone and the like to parse it and to return written keys.
    if engine == Engine::Postgres && !pins.is_empty() {
        let calls = (0..pins.len())
            .map(|i| {
                format!(
                    "pg_catalog.set_config(${}, ${}, false)",
                    2 * i + 1,
                    2 * i + 2
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        let params: Vec<Param> = pins
            .iter()
            .flat_map(|(name, value)| [Param::Text(name.clone()), Param::Text(value.clone())])
            .collect();
        session
            .run_bound(&format!("SELECT {calls}"), &params)
            .await?;
    }
    Ok(())
}

/// `mode` with STRICT_TRANS_TABLES appended unless it is already one of the comma-separated modes.
pub(crate) fn strict_sql_mode(mode: &str) -> String {
    if mode.split(',').any(|m| m == "STRICT_TRANS_TABLES") {
        mode.to_owned()
    } else if mode.is_empty() {
        "STRICT_TRANS_TABLES".to_owned()
    } else {
        format!("{mode},STRICT_TRANS_TABLES")
    }
}

/// The outcome of a change that ran: any Warning or Error row fails it with their messages
/// joined by "; " (Notes are ignored); else 0 rows is Conflict, 1 is Applied(1), more is Ambiguous.
pub(crate) fn change_outcome(matched: u64, warnings: &[(String, String)]) -> ChangeOutcome {
    let failures: Vec<&str> = warnings
        .iter()
        .filter(|(level, _)| level == "Warning" || level == "Error")
        .map(|(_, message)| message.as_str())
        .collect();
    if !failures.is_empty() {
        return ChangeOutcome::Failed(failures.join("; "));
    }
    match matched {
        0 => ChangeOutcome::Conflict,
        1 => ChangeOutcome::Applied(1),
        n => ChangeOutcome::Ambiguous(n),
    }
}

/// None lets the write run; otherwise the change's outcome, write not sent. No row is a
/// Conflict, several are Ambiguous, and one row is a Conflict unless every value is the same.
pub(crate) fn guard_outcome(
    rows: &[Vec<EditValue>],
    expected: &[EditValue],
) -> Option<ChangeOutcome> {
    match rows {
        [] => Some(ChangeOutcome::Conflict),
        [row] => {
            let held = row.len() == expected.len()
                && row.iter().zip(expected).all(|(a, b)| same_value(a, b));
            (!held).then_some(ChangeOutcome::Conflict)
        }
        many => Some(ChangeOutcome::Ambiguous(many.len() as u64)),
    }
}

/// Both NULL, or both non-null with equal bytes: text compares as its UTF-8 bytes, so a
/// value loaded as bytes still matches the same value read back as text.
fn same_value(a: &EditValue, b: &EditValue) -> bool {
    fn bytes(v: &EditValue) -> Option<&[u8]> {
        match v {
            EditValue::Text(s) => Some(s.as_bytes()),
            EditValue::Bytes(b) => Some(b),
            EditValue::Null | EditValue::Default => None,
        }
    }
    match (bytes(a), bytes(b)) {
        (Some(x), Some(y)) => x == y,
        (None, None) => *a == EditValue::Null && *b == EditValue::Null,
        (Some(_), None) | (None, Some(_)) => false,
    }
}

/// The message a failed change reports: the database's own words when it refused.
fn failure_message(e: ExecuteError) -> String {
    match e {
        ExecuteError::Database { message, .. } => message,
        other @ (ExecuteError::Timeout
        | ExecuteError::Cancelled
        | ExecuteError::ReadOnlyViolation { .. }
        | ExecuteError::Unsupported { .. }
        | ExecuteError::TransactionControl { .. }
        | ExecuteError::SessionClosed
        | ExecuteError::StoppedAtDrainCap
        | ExecuteError::Syntax { .. }
        | ExecuteError::ResultExpired
        | ExecuteError::SessionEnded { .. }) => other.to_string(),
    }
}
