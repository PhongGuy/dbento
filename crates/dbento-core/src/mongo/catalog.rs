//! The MongoDB catalog: databases, collections, columns inferred from a sample, and indexes.

use std::cmp::Reverse;
use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::Ordering;

use bson::{Bson, Document, doc};
use mongodb::action::Action;

use super::columns::decode;
use super::driver::MongoDriver;
use super::exec::read_preference;
use super::render;
use super::stream::cursor_of;
use crate::introspect::{
    ColumnMeta, IntrospectError, Relation, RelationKind, RelationList, RelationRef, SchemaInfo,
    capped,
};
use crate::objects::{IndexInfo, RelationObjects};
use crate::session::{ExecuteError, Writes};

const SAMPLE_SIZE: i32 = 100;
const SAMPLE_TIMEOUT_MS: i64 = 5_000;
const HIDDEN: [&str; 3] = ["admin", "config", "local"];
/// How `listIndexes` answers a missing collection and a view, neither of which has indexes.
const NO_INDEXES: [&str; 2] = ["NamespaceNotFound", "CommandNotSupportedOnView"];

pub(crate) async fn schemas(d: &MongoDriver) -> Result<Vec<SchemaInfo>, IntrospectError> {
    open(d)?;
    let admin = d.client.database("admin");
    let reply = admin
        .run_command(doc! { "listDatabases": 1, "nameOnly": true, "authorizedDatabases": true })
        .optional(read_preference(&admin, Writes::No), |call, criteria| {
            call.selection_criteria(criteria)
        })
        .await
        .map_err(|e| d.shared.failure(e, Writes::No))?;
    let entries = reply.get_array("databases").into_iter().flatten();
    let names = entries
        .filter_map(Bson::as_document)
        .filter_map(|entry| entry.get_str("name").ok())
        .map(str::to_owned)
        .collect();
    Ok(visible_schemas(names, &d.uri_database))
}

pub(crate) async fn relations(
    d: &MongoDriver,
    schema: &str,
) -> Result<RelationList, IntrospectError> {
    let command = doc! {
        "listCollections": 1,
        "nameOnly": true,
        "authorizedCollections": true,
    };
    Ok(relation_list(&cursor_documents(d, schema, command).await?))
}

pub(crate) async fn columns(
    d: &MongoDriver,
    rel: &RelationRef,
) -> Result<Vec<ColumnMeta>, IntrospectError> {
    let command = doc! {
        "aggregate": rel.name(),
        "pipeline": [{ "$sample": { "size": SAMPLE_SIZE } }],
        "cursor": {},
        "maxTimeMS": SAMPLE_TIMEOUT_MS,
    };
    let sample = cursor_documents(d, rel.schema(), command).await?;
    Ok(inferred_columns(&sample))
}

pub(crate) async fn relation_objects(
    d: &MongoDriver,
    rel: &RelationRef,
) -> Result<RelationObjects, IntrospectError> {
    let command = doc! { "listIndexes": rel.name() };
    let indexes = match cursor_documents(d, rel.schema(), command).await {
        Ok(entries) => entries.iter().filter_map(|e| index_info(rel, e)).collect(),
        Err(ExecuteError::Database { code, .. }) if NO_INDEXES.contains(&code.as_str()) => vec![],
        Err(e) => return Err(e.into()),
    };
    Ok(RelationObjects::new(indexes, vec![], vec![]))
}

fn open(d: &MongoDriver) -> Result<(), ExecuteError> {
    if d.shared.closed.load(Ordering::Relaxed) {
        return Err(ExecuteError::SessionClosed);
    }
    Ok(())
}

/// Every document of `command`'s cursor on `database`. Each one decodes lossily, as a grid row
/// does, so one stored invalid string cannot hide a whole collection's columns.
async fn cursor_documents(
    d: &MongoDriver,
    database: &str,
    command: Document,
) -> Result<Vec<Document>, ExecuteError> {
    open(d)?;
    let fail = |e| d.shared.failure(e, Writes::No);
    let database = d.client.database(database);
    let mut cursor = cursor_of(&database, command, Writes::No)
        .await
        .map_err(fail)?;
    let mut documents = Vec::new();
    while cursor.advance().await.map_err(fail)? {
        documents.push(decode(cursor.current())?);
    }
    Ok(documents)
}

/// `names` without the hidden three, except `default_db`, in name order.
fn visible_schemas(mut names: Vec<String>, default_db: &str) -> Vec<SchemaInfo> {
    names.retain(|name| name == default_db || !HIDDEN.contains(&name.as_str()));
    names.sort();
    names.into_iter().map(SchemaInfo::new).collect()
}

/// `listCollections` entries to relations in name order: `collection` and `timeseries` are
/// tables, `view` a view, `system.*` hidden.
fn relation_list(entries: &[Document]) -> RelationList {
    let mut items: Vec<Relation> = entries
        .iter()
        .filter_map(|entry| {
            let name = entry.get_str("name").ok()?;
            let kind = match entry.get_str("type") {
                Ok("view") => RelationKind::View,
                _ => RelationKind::Table,
            };
            (!name.starts_with("system.")).then(|| Relation::new(name, kind))
        })
        .collect();
    items.sort_by(|a, b| a.name().cmp(b.name()));
    capped(items)
}

/// Sampled documents to columns: `_id` first and the primary key, then by count, then first
/// seen; types by count, then name, joined by " | "; nullable if absent anywhere or null.
fn inferred_columns(documents: &[Document]) -> Vec<ColumnMeta> {
    #[derive(Default)]
    struct Field<'a> {
        name: &'a str,
        count: usize,
        types: BTreeMap<&'static str, usize>,
    }
    let mut fields: Vec<Field<'_>> = Vec::new();
    let mut slots: HashMap<&str, usize> = HashMap::new();
    for (name, value) in documents.iter().flatten() {
        let slot = *slots.entry(name).or_insert_with(|| {
            fields.push(Field {
                name,
                ..Field::default()
            });
            fields.len() - 1
        });
        let field = &mut fields[slot];
        field.count += 1;
        *field.types.entry(render::type_name(value)).or_default() += 1;
    }
    // Both sorts are stable, so first-seen order and the map's name order break their ties.
    fields.sort_by_key(|f| (f.name != "_id", Reverse(f.count)));
    fields
        .into_iter()
        .map(|field| {
            let nullable = field.count < documents.len() || field.types.contains_key("null");
            let mut types: Vec<_> = field.types.into_iter().collect();
            types.sort_by_key(|(_, count)| Reverse(*count));
            let types: Vec<_> = types.into_iter().map(|(name, _)| name).collect();
            let primary_key = (field.name == "_id").then_some(1);
            ColumnMeta::new(field.name, types.join(" | "), nullable, primary_key, None)
                .mark_inferred()
        })
        .collect()
}

/// A `listIndexes` entry.
fn index_info(rel: &RelationRef, entry: &Document) -> Option<IndexInfo> {
    let key = entry.get_document("key").ok()?;
    let name = entry.get_str("name").ok()?;
    let primary = name == "_id_";
    let unique = primary || matches!(entry.get("unique"), Some(Bson::Boolean(true)));
    let partial = entry.contains_key("partialFilterExpression");
    let method = key.values().find_map(Bson::as_str).map(str::to_owned);
    // A text index keys internal `_fts` and `_ftsx` fields; its `weights` name the real ones.
    let plain = key
        .keys()
        .filter(|k| !matches!(k.as_str(), "_fts" | "_ftsx"));
    let weighted = entry
        .get_document("weights")
        .into_iter()
        .flat_map(Document::keys);
    let columns = plain.chain(weighted).cloned().collect();
    Some(IndexInfo::new(
        rel.clone(),
        name,
        columns,
        unique,
        primary,
        partial,
        method,
    ))
}

#[cfg(test)]
mod tests;
