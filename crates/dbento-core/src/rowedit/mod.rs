//! Editing table data: the edit target a grid edits against and its row identity, the
//! staged changes against it, and the SQL that applies them.

mod apply;
mod browse;
mod sql;
mod target;
pub use apply::{Applied, ApplyError, ChangeOutcome, apply, apply_with};
pub use browse::{Browse, BrowseError, Keyset, Sort, browse_sql, key_filter};
use sql::Dialect;
pub(crate) use sql::Param;
pub use sql::render_sql;
pub use target::{EditColumn, EditTarget};
pub(crate) use target::{pg_edit_target, sqlite_edit_target};

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

/// Why a batch of changes was refused; each variant but `Empty` and `UnsupportedEngine`
/// names the bad change's index.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BatchError {
    /// The batch has no changes.
    #[error("the batch has no changes")]
    Empty,
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
}

impl EditBatch {
    /// Refuses an engine that cannot edit rows, then an empty batch, validates `changes`
    /// against `target` and drops values for non-editable columns, so generating and
    /// applying the batch cannot fail on its shape.
    pub fn new(target: EditTarget, changes: Vec<Change>) -> Result<EditBatch, BatchError> {
        let engine = target.engine();
        let Some(dialect) = Dialect::of(engine).filter(|_| engine.capabilities().edit_rows) else {
            return Err(BatchError::UnsupportedEngine { engine });
        };
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
        Change::Update { key, set } => {
            check_key(target, change, &key)?;
            let set = editable_values(target, change, set)?;
            if set.is_empty() {
                return Err(BatchError::EmptyUpdate { change });
            }
            Change::Update { key, set }
        }
        Change::Insert { values } => Change::Insert {
            values: editable_values(target, change, values)?,
        },
        Change::Delete { key } => {
            check_key(target, change, &key)?;
            Change::Delete { key }
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
    let mut kept = Vec::with_capacity(values.len());
    for (i, (column, value)) in values.iter().enumerate() {
        let Some(col) = target.column(column) else {
            return Err(BatchError::UnknownColumn {
                change,
                column: column.clone(),
            });
        };
        if values[..i].iter().any(|(c, _)| c == column) {
            return Err(BatchError::DuplicateColumn {
                change,
                column: column.clone(),
            });
        }
        if col.editable() {
            kept.push((column.clone(), value.clone()));
        }
    }
    Ok(kept)
}

#[cfg(test)]
mod tests;
