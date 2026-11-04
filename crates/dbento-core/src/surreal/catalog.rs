//! The SurrealDB catalog: reads the `INFO` replies and sampled rows into catalog types.

use super::decode::Wire;
use super::shape::Shape;
use super::wire::field;
use crate::introspect::{
    ColumnMeta, IntrospectError, Relation, RelationKind, RelationList, RelationRef, SchemaInfo,
    capped,
};
use crate::objects::{IndexInfo, RelationObjects, TriggerInfo, TriggerState};

const NS: &str = "INFO FOR NS";
const DB: &str = "INFO FOR DB STRUCTURE";
const TABLE: &str = "INFO FOR TABLE STRUCTURE";

/// What a `tables` entry of `INFO FOR DB STRUCTURE` says about one table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TableInfo {
    pub(crate) schemafull: bool,
    pub(crate) view: bool,
}

/// `INFO FOR NS`: the keys of `databases`, in name order.
pub(crate) fn schema_names(ns: &Wire) -> Result<Vec<SchemaInfo>, IntrospectError> {
    let Some(Wire::Object(databases)) = field(ns, "databases") else {
        return Err(changed(NS, "databases", "an object"));
    };
    let mut names: Vec<&str> = databases.iter().map(|(name, _)| name.as_str()).collect();
    names.sort_unstable();
    Ok(names.into_iter().map(SchemaInfo::new).collect())
}

/// `INFO FOR DB STRUCTURE`: `tables`, a `view` key making a View, in name order, capped.
pub(crate) fn relation_list(db: &Wire) -> Result<RelationList, IntrospectError> {
    let mut items = Vec::new();
    for entry in list_of(db, "tables", DB)? {
        let kind = match field(entry, "view") {
            Some(_) => RelationKind::View,
            None => RelationKind::Table,
        };
        items.push(Relation::new(text_of(entry, "name", DB)?, kind));
    }
    items.sort_by(|a, b| a.name().cmp(b.name()));
    Ok(capped(items))
}

/// The `tables` entry named `name`; None when there is none.
pub(crate) fn table_info(db: &Wire, name: &str) -> Result<Option<TableInfo>, IntrospectError> {
    for entry in list_of(db, "tables", DB)? {
        if text_of(entry, "name", DB)? != name {
            continue;
        }
        let Some(Wire::Bool(schemafull)) = field(entry, "schemafull") else {
            return Err(changed(DB, "schemafull", "a bool"));
        };
        let view = field(entry, "view").is_some();
        return Ok(Some(TableInfo {
            schemafull: *schemafull,
            view,
        }));
    }
    Ok(None)
}

/// `INFO FOR TABLE $t STRUCTURE` `fields` of table `name`: `id` first as the primary key, typed
/// by its declared kind or `record<name>`, then every other field in the server's order.
pub(crate) fn declared_columns(
    table: &Wire,
    name: &str,
) -> Result<Vec<ColumnMeta>, IntrospectError> {
    let mut id_kind = None;
    let mut columns = Vec::new();
    for entry in list_of(table, "fields", TABLE)? {
        let column = text_of(entry, "name", TABLE)?;
        let kind = optional_text(entry, "kind");
        if column == "id" {
            id_kind = kind;
            continue;
        }
        let default = optional_text(entry, "default").map(str::to_owned);
        let nullable = kind.is_none_or(nullable);
        columns.push(ColumnMeta::new(
            column,
            kind.unwrap_or("any"),
            nullable,
            None,
            default,
        ));
    }
    let id_type = id_kind.map_or_else(|| format!("record<{name}>"), str::to_owned);
    columns.insert(0, ColumnMeta::new("id", id_type, false, Some(1), None));
    Ok(columns)
}

/// `declared` plus the sample's other keys, typed by their common value type, nullable and
/// marked inferred, in the sample's key order.
pub(crate) fn with_inferred(mut declared: Vec<ColumnMeta>, rows: &[Wire]) -> Vec<ColumnMeta> {
    // Rows that are not all objects name no keys, and Shape would give them one `value` column.
    if !rows.iter().all(|row| matches!(row, Wire::Object(_))) {
        return declared;
    }
    let (_, sampled) = Shape::of(rows, false, true);
    for column in sampled {
        if declared.iter().any(|d| d.name() == column.name()) {
            continue;
        }
        let meta = ColumnMeta::new(column.name(), column.type_name(), true, None, None);
        declared.push(meta.mark_inferred());
    }
    declared
}

/// `.indexes` and `.events` of an `INFO FOR TABLE $t STRUCTURE` reply: each index keeps its
/// `index` text as the method unless it is empty or `UNIQUE`, and each event is an enabled
/// trigger. SurrealDB has no constraints to list.
pub(crate) fn relation_objects(
    rel: &RelationRef,
    table: &Wire,
) -> Result<RelationObjects, IntrospectError> {
    let mut indexes = Vec::new();
    for entry in list_of(table, "indexes", TABLE)? {
        let name = text_of(entry, "name", TABLE)?;
        let columns = list_of(entry, "cols", TABLE)?
            .iter()
            .map(|col| match col {
                Wire::Text(col) => Ok(col.clone()),
                _ => Err(changed(TABLE, "cols", "a list of text")),
            })
            .collect::<Result<Vec<_>, _>>()?;
        let kind = text_of(entry, "index", TABLE)?;
        let unique = kind == "UNIQUE";
        let method = (!kind.is_empty() && !unique).then(|| kind.to_owned());
        indexes.push(IndexInfo::new(
            rel.clone(),
            name,
            columns,
            unique,
            false,
            false,
            method,
        ));
    }
    let mut triggers = Vec::new();
    for entry in list_of(table, "events", TABLE)? {
        let name = text_of(entry, "name", TABLE)?;
        triggers.push(TriggerInfo::new(rel.clone(), name, TriggerState::Enabled));
    }
    Ok(RelationObjects::new(indexes, vec![], triggers))
}

/// Whether a field of this kind accepts NONE or NULL: `any`, `option<..>`, or a top-level `|`
/// part that is `none` or `null`.
fn nullable(kind: &str) -> bool {
    let kind = kind.trim();
    if kind == "any" || kind.starts_with("option<") {
        return true;
    }
    // Split on `|` outside `<..>`, so a nested `array<int | none>` is not read as nullable.
    let mut depth = 0usize;
    let mut start = 0;
    let mut parts = Vec::new();
    for (at, c) in kind.char_indices() {
        match c {
            '<' => depth += 1,
            '>' => depth = depth.saturating_sub(1),
            '|' if depth == 0 => {
                parts.push(&kind[start..at]);
                start = at + 1;
            }
            _ => {}
        }
    }
    parts.push(&kind[start..]);
    parts
        .iter()
        .any(|part| matches!(part.trim(), "none" | "null"))
}

fn changed(call: &str, key: &str, what: &str) -> IntrospectError {
    IntrospectError::Unexpected {
        message: format!("{call} changed shape: `{key}` is not {what}"),
    }
}

fn list_of<'w>(value: &'w Wire, key: &str, call: &str) -> Result<&'w [Wire], IntrospectError> {
    match field(value, key) {
        Some(Wire::Array(items)) => Ok(items),
        _ => Err(changed(call, key, "a list")),
    }
}

fn text_of<'w>(value: &'w Wire, key: &str, call: &str) -> Result<&'w str, IntrospectError> {
    optional_text(value, key).ok_or_else(|| changed(call, key, "text"))
}

fn optional_text<'w>(value: &'w Wire, key: &str) -> Option<&'w str> {
    match field(value, key) {
        Some(Wire::Text(text)) => Some(text),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
