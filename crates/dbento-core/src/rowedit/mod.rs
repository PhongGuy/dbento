//! Editing table data: the edit target a grid edits against and its row identity, the
//! staged changes against it, and the SQL that applies them.

mod apply;
mod browse;
mod reread;
mod sql;
mod target;
pub use apply::{Applied, ApplyError, ChangeOutcome, apply, apply_with};
pub use browse::{Browse, BrowseError, Keyset, Sort, browse_sql, key_filter};
pub use reread::{RereadError, reread};
use sql::Dialect;
pub(crate) use sql::Param;
pub use sql::render_sql;
pub use target::{EditColumn, EditTarget, NotEditable};
pub(crate) use target::{mysql_edit_target, mysql_inexact, pg_edit_target, sqlite_edit_target};

/// One value written to or matched in a cell.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum EditValue {
    /// SQL NULL.
    Null,
    /// The column's default; never valid in a row key.
    Default,
    /// A value in the engine's text form, converted by the database.
    Text(String),
    /// Raw bytes, for binary columns.
    Bytes(Vec<u8>),
}

/// The identity values of one row, named in the target's identity order.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RowKey {
    values: Vec<(String, EditValue)>,
}

impl RowKey {
    /// A key from `(column, value)` pairs in identity order.
    pub fn new(values: Vec<(String, EditValue)>) -> Self {
        Self { values }
    }

    /// The `(column, value)` pairs.
    pub fn values(&self) -> &[(String, EditValue)] {
        &self.values
    }
}

/// One staged row change.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Change {
    /// Set columns of the row matched by `key`.
    Update {
        /// The row changed.
        key: RowKey,
        /// The `(column, value)` pairs written.
        set: Vec<(String, EditValue)>,
        /// `(column, loaded value)` pairs the row must still hold; empty checks the key only.
        expected: Vec<(String, EditValue)>,
    },
    /// A new row; columns not listed take their default.
    Insert {
        /// The `(column, value)` pairs written.
        values: Vec<(String, EditValue)>,
    },
    /// Remove the row matched by `key`.
    Delete {
        /// The row removed.
        key: RowKey,
        /// `(column, loaded value)` pairs the row must still hold; empty checks the key only.
        expected: Vec<(String, EditValue)>,
    },
}

/// A validated set of changes against one edit target.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct EditBatch {
    target: EditTarget,
    changes: Vec<Change>,
    dialect: Dialect,
}

/// Why a batch of changes was refused; each variant but `Empty`, `NotEditable` and
/// `UnsupportedEngine` names the bad change's index.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BatchError {
    /// The batch has no changes.
    #[error("the batch has no changes")]
    Empty,
    /// The target was described as not editable, so no change of any kind may reach it.
    #[error("the table cannot be edited: {reason}")]
    NotEditable {
        /// Why the target cannot be edited.
        reason: NotEditable,
    },
    /// The target's engine cannot edit rows.
    #[error("editing rows is not available on {} yet", engine.name())]
    UnsupportedEngine {
        /// The target's engine.
        engine: crate::workspace::Engine,
    },
    /// An Update or Delete on a target without row identity.
    #[error("change {change}: the table has no row identity, so rows cannot be updated or deleted")]
    NoIdentity {
        /// Index of the change.
        change: usize,
    },
    /// The key's column names differ from the identity, in order.
    #[error("change {change}: the row key must name {expected:?} in that order")]
    KeyMismatch {
        /// Index of the change.
        change: usize,
        /// The identity columns expected.
        expected: Vec<String>,
    },
    /// A key value is DEFAULT.
    #[error("change {change}: a row key cannot hold DEFAULT")]
    DefaultInKey {
        /// Index of the change.
        change: usize,
    },
    /// A name that is not a column of the target, locators included.
    #[error("change {change}: {column:?} is not a column of the table")]
    UnknownColumn {
        /// Index of the change.
        change: usize,
        /// The name given.
        column: String,
    },
    /// A column given twice in one change.
    #[error("change {change}: column {column:?} is given twice")]
    DuplicateColumn {
        /// Index of the change.
        change: usize,
        /// The repeated column.
        column: String,
    },
    /// An Update with nothing left to set.
    #[error("change {change}: the update sets no editable column")]
    EmptyUpdate {
        /// Index of the change.
        change: usize,
    },
    /// An expected value is DEFAULT, which no row holds.
    #[error("change {change}: an expected value cannot be DEFAULT")]
    DefaultExpected {
        /// Index of the change.
        change: usize,
    },
}

impl EditBatch {
    /// Refuses an engine that cannot edit rows, then a target with a not-editable reason,
    /// then an empty batch, validates `changes` against `target` and drops values for
    /// non-editable columns, so generating and applying the batch cannot fail on its shape.
    pub fn new(target: EditTarget, changes: Vec<Change>) -> Result<EditBatch, BatchError> {
        let engine = target.engine();
        let Some(dialect) = Dialect::of(engine).filter(|_| engine.capabilities().edit_rows) else {
            return Err(BatchError::UnsupportedEngine { engine });
        };
        // Every column of a refused target is read-only, so an Insert would otherwise
        // shrink to an all-default row that a non-transactional table cannot roll back.
        if let Some(reason) = target.not_editable() {
            return Err(BatchError::NotEditable {
                reason: reason.clone(),
            });
        }
        if changes.is_empty() {
            return Err(BatchError::Empty);
        }
        let changes = changes
            .into_iter()
            .enumerate()
            .map(|(i, change)| normalize(&target, i, change))
            .collect::<Result<_, _>>()?;
        Ok(EditBatch {
            target,
            changes,
            dialect,
        })
    }

    /// The target the changes apply to.
    pub fn target(&self) -> &EditTarget {
        &self.target
    }

    /// The changes in order, with non-editable columns already dropped.
    pub fn changes(&self) -> &[Change] {
        &self.changes
    }

    /// The SQL dialect the batch's statements are generated in.
    pub(crate) fn dialect(&self) -> Dialect {
        self.dialect
    }
}

fn normalize(target: &EditTarget, change: usize, c: Change) -> Result<Change, BatchError> {
    Ok(match c {
        Change::Update { key, set, expected } => {
            check_key(target, change, &key)?;
            let set = editable_values(target, change, set)?;
            if set.is_empty() {
                return Err(BatchError::EmptyUpdate { change });
            }
            check_expected(target, change, &expected)?;
            Change::Update { key, set, expected }
        }
        Change::Insert { values } => Change::Insert {
            values: editable_values(target, change, values)?,
        },
        Change::Delete { key, expected } => {
            check_key(target, change, &key)?;
            check_expected(target, change, &expected)?;
            Change::Delete { key, expected }
        }
    })
}

/// A key must name exactly the identity columns in identity order and hold no DEFAULT.
fn check_key(target: &EditTarget, change: usize, key: &RowKey) -> Result<(), BatchError> {
    if !target.has_identity() {
        return Err(BatchError::NoIdentity { change });
    }
    if !key.values().iter().map(|(c, _)| c).eq(target.identity()) {
        return Err(BatchError::KeyMismatch {
            change,
            expected: target.identity().to_vec(),
        });
    }
    if key.values().iter().any(|(_, v)| *v == EditValue::Default) {
        return Err(BatchError::DefaultInKey { change });
    }
    Ok(())
}

/// Every name must be a distinct target column; values for non-editable ones are dropped.
fn editable_values(
    target: &EditTarget,
    change: usize,
    values: Vec<(String, EditValue)>,
) -> Result<Vec<(String, EditValue)>, BatchError> {
    check_columns(target, change, &values)?;
    Ok(values
        .into_iter()
        .filter(|(column, _)| target.column(column).is_some_and(|c| c.editable()))
        .collect())
}

/// Expected values are kept for every column, non-editable ones included, since the
/// row must still hold them all; a DEFAULT is refused because no row holds it.
fn check_expected(
    target: &EditTarget,
    change: usize,
    expected: &[(String, EditValue)],
) -> Result<(), BatchError> {
    check_columns(target, change, expected)?;
    if expected.iter().any(|(_, v)| *v == EditValue::Default) {
        return Err(BatchError::DefaultExpected { change });
    }
    Ok(())
}

/// Every name must be a distinct target column, locators excluded.
fn check_columns(
    target: &EditTarget,
    change: usize,
    values: &[(String, EditValue)],
) -> Result<(), BatchError> {
    for (i, (column, _)) in values.iter().enumerate() {
        if target.column(column).is_none() {
            return Err(BatchError::UnknownColumn {
                change,
                column: column.clone(),
            });
        }
        if values[..i].iter().any(|(c, _)| c == column) {
            return Err(BatchError::DuplicateColumn {
                change,
                column: column.clone(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests;
