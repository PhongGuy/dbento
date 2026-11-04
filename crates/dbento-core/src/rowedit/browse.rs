//! The SELECT a grid browses an edit target with: locators first, an optional user filter,
//! one sort column and, past a parked page, a keyset continuation.

use super::sql::pg_hex;
use super::{EditTarget, EditValue, RowKey};
use crate::Engine;
use crate::introspect::{quote_ident, quote_literal};
use crate::ranges::statement_ranges;

/// How a target is browsed: an optional WHERE filter and an optional sort.
#[non_exhaustive]
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Browse {
    /// SQL placed inside `WHERE (...)`; blank counts as no filter.
    pub filter: Option<String>,
    /// The column the rows are ordered by.
    pub sort: Option<Sort>,
    /// Continue after this key.
    pub after: Option<Keyset>,
}

impl Browse {
    /// A browse with the given filter and sort.
    pub fn new(filter: Option<String>, sort: Option<Sort>) -> Self {
        Self {
            filter,
            sort,
            after: None,
        }
    }

    /// The same browse, continued past the row keyed `after`.
    pub fn with_after(self, after: Keyset) -> Self {
        Self {
            after: Some(after),
            ..self
        }
    }
}

/// The key of the last row already loaded; an unsorted browse resumes after it.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keyset {
    key: RowKey,
}

impl Keyset {
    /// A continuation after the row keyed `key`.
    pub fn new(key: RowKey) -> Self {
        Self { key }
    }

    /// The key rows must follow.
    pub fn key(&self) -> &RowKey {
        &self.key
    }
}

/// One sort column and its direction.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sort {
    /// A column or locator of the target.
    pub column: String,
    /// Sort descending instead of ascending.
    pub descending: bool,
}

impl Sort {
    /// A sort on `column`.
    pub fn new(column: impl Into<String>, descending: bool) -> Self {
        Self {
            column: column.into(),
            descending,
        }
    }
}

/// Why browse SQL could not be built.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BrowseError {
    /// The filter ends the statement or starts another one.
    #[error("the filter must stay inside one statement")]
    MultipleStatements,
    /// The filter leaves a comment, quoted text or dollar quote open.
    #[error("the filter leaves a comment or quoted text open")]
    Unterminated,
    /// The sort column's type has no ordering, so the engine would refuse the browse.
    #[error("column {column} cannot be sorted")]
    Unsortable {
        /// The requested column.
        column: String,
    },
    /// The sort column is not a column or locator of the target.
    #[error("no column {column} to sort by")]
    UnknownSort {
        /// The requested column.
        column: String,
    },
    /// The keyset is on a sorted browse, a target without a stable key, or a key that does
    /// not name the identity columns with plain values.
    #[error("a keyset continuation needs an unsorted browse keyed by its primary key or rowid")]
    Keyset,
}

/// The SELECT that browses `target`, one clause per line, refusing a filter that would make
/// it more than one statement or leave a comment or quote open, as the engine's splitter sees it.
/// On a stable-key target the order ends with the key, so pages split at the same rows.
pub fn browse_sql(target: &EditTarget, browse: &Browse) -> Result<String, BrowseError> {
    let engine = target.engine();
    let ident = |name: &str| quote_ident(engine, name);
    let keyset = browse
        .after
        .as_ref()
        .map(|k| keyset_condition(target, browse, k))
        .transpose()?;
    let rel = target.relation();
    let only = if target.only() { "ONLY " } else { "" };
    let mut sql = String::from("SELECT ");
    for locator in target.locators() {
        sql.push_str(&ident(locator));
        sql.push_str(", ");
    }
    sql.push_str(&format!(
        "*\nFROM {only}{}.{}",
        ident(rel.schema()),
        ident(rel.name())
    ));
    // The filter goes in untrimmed on its own lines so a trailing `--` comment ends before `)`.
    let filter = browse.filter.as_deref().filter(|f| !f.trim().is_empty());
    if let Some(filter) = filter {
        sql.push_str(&format!("\nWHERE (\n{filter}\n)"));
    }
    if let Some(keyset) = keyset {
        let joiner = if filter.is_some() { "AND" } else { "WHERE" };
        sql.push_str(&format!("\n{joiner} {keyset}"));
    }
    let mut order = Vec::new();
    if let Some(sort) = &browse.sort {
        let known =
            target.column(&sort.column).is_some() || target.locators().contains(&sort.column);
        if !known {
            return Err(BrowseError::UnknownSort {
                column: sort.column.clone(),
            });
        }
        if !target.sortable(&sort.column) {
            return Err(BrowseError::Unsortable {
                column: sort.column.clone(),
            });
        }
        let dir = if sort.descending { "DESC" } else { "ASC" };
        order.push(format!("{} {dir}", ident(&sort.column)));
    }
    if target.stable_key() {
        let sorted = browse.sort.as_ref().map(|s| s.column.as_str());
        for key in target
            .identity()
            .iter()
            .filter(|k| Some(k.as_str()) != sorted)
        {
            order.push(format!("{} ASC", ident(key)));
        }
    }
    if !order.is_empty() {
        sql.push_str(&format!("\nORDER BY {}", order.join(", ")));
    }
    if statement_ranges(engine, &sql).len() != 1 {
        return Err(BrowseError::MultipleStatements);
    }
    // A filter that leaves a comment or quote open would swallow whatever follows it, so a
    // probe statement appended after the browse must still split off on its own.
    if statement_ranges(engine, &format!("{sql};\nSELECT 1")).len() != 2 {
        return Err(BrowseError::Unterminated);
    }
    Ok(sql)
}

/// `col = literal AND ...` with quoted columns and untyped literals, so the server resolves
/// each column's type. None when `pairs` is empty or holds a Null or Default.
pub fn key_filter(engine: Engine, pairs: &[(String, EditValue)]) -> Option<String> {
    if pairs.is_empty() {
        return None;
    }
    let mut terms = Vec::with_capacity(pairs.len());
    for (column, value) in pairs {
        let literal = literal(engine, value)?;
        terms.push(format!("{} = {literal}", quote_ident(engine, column)));
    }
    Some(terms.join(" AND "))
}

/// `(k1, k2) > (l1, l2)` after the keyset's key, refused unless the browse is unsorted, the
/// target has a stable key, and the key names its identity columns in order with plain values.
fn keyset_condition(
    target: &EditTarget,
    browse: &Browse,
    keyset: &Keyset,
) -> Result<String, BrowseError> {
    let engine = target.engine();
    let pairs = keyset.key().values();
    let named = pairs.iter().map(|(c, _)| c).eq(target.identity());
    if browse.sort.is_some() || !target.stable_key() || !named {
        return Err(BrowseError::Keyset);
    }
    let literals = pairs
        .iter()
        .map(|(_, v)| literal(engine, v))
        .collect::<Option<Vec<_>>>()
        .ok_or(BrowseError::Keyset)?;
    let columns: Vec<String> = pairs.iter().map(|(c, _)| quote_ident(engine, c)).collect();
    Ok(format!(
        "({}) > ({})",
        columns.join(", "),
        literals.join(", ")
    ))
}

/// An untyped literal for `value`, so the server resolves it against the column's type; None
/// for Null and Default, which match no row, and for bytes on SQL Server, which has no byte
/// literal here yet.
fn literal(engine: Engine, value: &EditValue) -> Option<String> {
    Some(match (value, engine) {
        (EditValue::Text(t), _) => quote_literal(engine, t),
        (EditValue::Bytes(b), Engine::Sqlite | Engine::Mysql) => {
            format!("X'{}'", &pg_hex(b)[2..])
        }
        (EditValue::Bytes(_), Engine::Mssql) => return None,
        (EditValue::Bytes(b), _) => quote_literal(engine, &pg_hex(b)),
        _ => return None,
    })
}
