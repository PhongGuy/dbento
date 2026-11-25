//! Re-reading rows a grid loaded, in the text a guard compares, so a caller can show what
//! the database holds now and take it as the new loaded values.

use super::apply::pin_session;
use super::sql::{Dialect, read_row};
use super::{EditTarget, EditValue, RowKey, check_key};
use crate::Secret;
use crate::session::{ConnectError, ExecuteError, Session, SessionOptions, connect};
use crate::workspace::{ConnectionConfig, Workspace};

/// Why rows could not be re-read.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum RereadError {
    /// The target's engine cannot edit rows; nothing was opened.
    #[error("editing rows is not available on {} yet", engine.name())]
    UnsupportedEngine {
        /// The target's engine.
        engine: crate::workspace::Engine,
    },
    /// The private read-only session could not be opened.
    #[error(transparent)]
    Connect(#[from] ConnectError),
    /// A session setting or a read failed.
    #[error("reading the rows failed: {0}")]
    Query(ExecuteError),
}

/// Reads the rows at `keys` under the target's pins, in the guard's text form, without a lock.
/// One entry per key: None when no row holds it, else the target's columns in target order.
/// A key that does not name the identity in order, or holds DEFAULT, is None without a
/// query; a key matching several rows gives the first. The session is always read-only.
pub async fn reread(
    ws: &Workspace,
    conn: &ConnectionConfig,
    secret: Option<Secret>,
    options: SessionOptions,
    target: &EditTarget,
    keys: &[RowKey],
) -> Result<Vec<Option<Vec<(String, EditValue)>>>, RereadError> {
    let engine = target.engine();
    let Some(dialect) = Dialect::of(engine).filter(|_| engine.capabilities().edit_rows) else {
        return Err(RereadError::UnsupportedEngine { engine });
    };
    let options = SessionOptions {
        force_read_only: true,
        ..options
    };
    let mut session = connect(ws, conn, secret, options).await?;
    let rows = read_rows(&mut session, conn, dialect, target, keys).await;
    let _ = session.close().await;
    rows.map_err(RereadError::Query)
}

/// Pins the session as `apply` does, so values read back in the browse's text, then reads
/// each key's row outside any transaction.
async fn read_rows(
    session: &mut Session,
    conn: &ConnectionConfig,
    dialect: Dialect,
    target: &EditTarget,
    keys: &[RowKey],
) -> Result<Vec<Option<Vec<(String, EditValue)>>>, ExecuteError> {
    pin_session(session, conn.engine(), target.pins()).await?;
    let names: Vec<String> = target
        .columns()
        .iter()
        .map(|c| c.name().to_owned())
        .collect();
    let mut rows = Vec::with_capacity(keys.len());
    for key in keys {
        if check_key(target, 0, key).is_err() {
            rows.push(None);
            continue;
        }
        let read = read_row(target, dialect, key);
        let found = session.query_bound(&read.sql, &read.params).await?;
        rows.push(
            found
                .into_iter()
                .next()
                .map(|values| names.iter().cloned().zip(values).collect()),
        );
    }
    Ok(rows)
}
