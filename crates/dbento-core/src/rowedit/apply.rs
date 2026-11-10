//! Applying a batch: every change runs in one transaction on a private session, and the
//! caller sees each change's outcome before choosing to commit or roll back.

use std::fmt;

use super::sql::{Param, bound};
use super::{EditBatch, RowKey};
use crate::Secret;
use crate::session::{ConnectError, ExecuteError, Session, SessionOptions, connect};
use crate::workspace::{ConnectionConfig, Engine, Workspace};

/// What one change did inside the apply transaction.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeOutcome {
    /// The change touched this many rows (one, for a batch that can commit).
    Applied(u64),
    /// The row was not found: it was changed or removed by someone else.
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
    // On Postgres a row locked elsewhere fails the change after five seconds instead of
    // hanging; the session is private to this apply, so a session-level SET is enough.
    // SQLite waits on the 5000 ms busy timeout its connection already has.
    let mut started = Ok(0);
    if conn.engine() == Engine::Postgres {
        started = session.run_bound("SET lock_timeout = '5s'", &[]).await;
    }
    // Key text is the browse session's output text, so the apply session takes on that
    // session's DateStyle, TimeZone and the like to parse it and to return written keys.
    let pins = batch.target().pins();
    if started.is_ok() && !pins.is_empty() {
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
        started = session.run_bound(&format!("SELECT {calls}"), &params).await;
    }
    if started.is_ok() {
        started = session.run_bound("BEGIN", &[]).await;
    }
    if let Err(e) = started {
        let _ = session.close().await;
        return Err(ApplyError::Session(e));
    }
    // Conflicts and ambiguities leave the transaction usable, so the run continues to
    // report every row; a failure aborts it, so later changes are not sent.
    let mut outcomes = Vec::with_capacity(batch.changes().len());
    let mut keys = Vec::with_capacity(batch.changes().len());
    let mut failed = false;
    for statement in bound(&batch) {
        if failed {
            outcomes.push(ChangeOutcome::NotAttempted);
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
        let mut key = None;
        outcomes.push(match ran {
            Ok((0, _)) => ChangeOutcome::Conflict,
            Ok((1, row)) => {
                key = row.map(|values| {
                    RowKey::new(
                        batch
                            .target()
                            .identity()
                            .iter()
                            .cloned()
                            .zip(values)
                            .collect(),
                    )
                });
                ChangeOutcome::Applied(1)
            }
            Ok((n, _)) => ChangeOutcome::Ambiguous(n),
            Err(e) => {
                failed = true;
                ChangeOutcome::Failed(match e {
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
                })
            }
        });
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
