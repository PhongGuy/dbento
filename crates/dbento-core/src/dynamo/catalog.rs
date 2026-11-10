//! The DynamoDB catalog: one schema per region, its tables, their key attributes and indexes,
//! and the other attributes as a sample of items shows them.

use std::cmp::Reverse;
use std::collections::BTreeMap;
use std::time::Duration;

use aws_sdk_dynamodb::types::{KeySchemaElement, KeyType};

use super::client::DynamoClient;
use super::errors::execute_error;
use super::render;
use crate::introspect::{
    ColumnMeta, IntrospectError, RELATION_CAP, Relation, RelationKind, RelationList, RelationRef,
    SchemaInfo, capped,
};
use crate::objects::{IndexInfo, RelationObjects};
use crate::session::ExecuteError;

/// How many items the attribute sample reads.
pub(crate) const SAMPLE_SIZE: i32 = 100;
/// How long the attribute sample may take before `columns` fails.
pub(crate) const SAMPLE_TIMEOUT: Duration = Duration::from_secs(5);

/// A table's declared shape: its key attributes, their types and its secondary indexes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TableKeys {
    pub(crate) hash: String,
    pub(crate) range: Option<String>,
    /// Every attribute the table or an index keys on, with its type (S, N or B).
    pub(crate) types: BTreeMap<String, String>,
    /// Name, "GSI" or "LSI", and key attributes, GSIs first.
    pub(crate) indexes: Vec<(String, &'static str, Vec<String>)>,
}

impl TableKeys {
    /// The primary key's attributes: the hash key, then the range key if there is one.
    pub(crate) fn key_names(&self) -> Vec<String> {
        std::iter::once(self.hash.clone())
            .chain(self.range.clone())
            .collect()
    }
}

/// The table's keys and indexes, or `None` when the table does not exist.
pub(crate) async fn describe(
    client: &DynamoClient,
    table: &str,
) -> Result<Option<TableKeys>, ExecuteError> {
    let output = match client.describe_table(table).await.map_err(execute_error) {
        Ok(output) => output,
        Err(ExecuteError::Database { code, .. }) if code == "ResourceNotFoundException" => {
            return Ok(None);
        }
        Err(e) => return Err(e),
    };
    let unexpected = |what: &str| {
        ExecuteError::database("DescribeTable", format!("table `{table}` has no {what}"))
    };
    let desc = output.table().ok_or_else(|| unexpected("description"))?;
    let (hash, range) = key_of(desc.key_schema());
    let types = desc
        .attribute_definitions()
        .iter()
        .map(|d| {
            (
                d.attribute_name().to_owned(),
                d.attribute_type().as_str().to_owned(),
            )
        })
        .collect();
    let gsis = desc
        .global_secondary_indexes()
        .iter()
        .map(|i| (i.index_name(), "GSI", i.key_schema()));
    let lsis = desc
        .local_secondary_indexes()
        .iter()
        .map(|i| (i.index_name(), "LSI", i.key_schema()));
    let indexes = gsis
        .chain(lsis)
        .map(|(name, kind, schema)| {
            let (hash, range) = key_of(schema);
            let attrs = hash.into_iter().chain(range).collect();
            (name.unwrap_or("unknown").to_owned(), kind, attrs)
        })
        .collect();
    Ok(Some(TableKeys {
        hash: hash.ok_or_else(|| unexpected("HASH key"))?,
        range,
        types,
        indexes,
    }))
}

/// The HASH and RANGE attributes of a key schema.
fn key_of(schema: &[KeySchemaElement]) -> (Option<String>, Option<String>) {
    let find = |kind: KeyType| {
        schema
            .iter()
            .find(|e| *e.key_type() == kind)
            .map(|e| e.attribute_name().to_owned())
    };
    (find(KeyType::Hash), find(KeyType::Range))
}

/// The one schema: the connection's region.
pub(crate) fn schemas(region: &str) -> Vec<SchemaInfo> {
    vec![SchemaInfo::new(region)]
}

/// Every table of the region, in name order; any other schema has none and sends nothing.
pub(crate) async fn relations(
    client: &DynamoClient,
    region: &str,
    schema: &str,
) -> Result<RelationList, IntrospectError> {
    if schema != region {
        return Ok(RelationList::new(vec![], false));
    }
    let mut names = Vec::new();
    let mut start: Option<String> = None;
    // One name past the cap is enough for `capped` to mark the list truncated.
    loop {
        let page = client
            .list_tables(start.as_deref(), None)
            .await
            .map_err(execute_error)?;
        names.extend(page.table_names().iter().cloned());
        start = page.last_evaluated_table_name().map(str::to_owned);
        if start.is_none() || names.len() > RELATION_CAP {
            break;
        }
    }
    names.sort();
    let items = names
        .into_iter()
        .map(|n| Relation::new(n, RelationKind::Table))
        .collect();
    Ok(capped(items))
}

/// The key attributes as declared, then every other attribute a sample of items holds, most
/// common first, with its observed types. A failed or slow sample fails the whole call.
pub(crate) async fn columns(
    client: &DynamoClient,
    table: &str,
    keys: &TableKeys,
) -> Result<Vec<ColumnMeta>, IntrospectError> {
    let sample = tokio::time::timeout(SAMPLE_TIMEOUT, client.scan(table, SAMPLE_SIZE))
        .await
        .map_err(|_| ExecuteError::Timeout)?
        .map_err(execute_error)?;
    let key_names = keys.key_names();
    let declared = key_names.iter().zip(1..).map(|(name, position)| {
        let type_name = keys
            .types
            .get(name)
            .cloned()
            .unwrap_or_else(|| "unknown".to_owned());
        ColumnMeta::new(name, type_name, false, Some(position), None)
    });

    #[derive(Default)]
    struct Seen {
        count: usize,
        types: BTreeMap<&'static str, usize>,
    }
    let items = sample.items();
    let mut seen: BTreeMap<String, Seen> = BTreeMap::new();
    for item in items {
        for (name, value) in render::item_to_typed(item) {
            if key_names.contains(&name) {
                continue;
            }
            let entry = seen.entry(name).or_default();
            entry.count += 1;
            *entry.types.entry(render::type_name(&value)).or_default() += 1;
        }
    }
    // Stable sorts over name-ordered maps, so equal counts fall back to name order.
    let mut seen: Vec<_> = seen.into_iter().collect();
    seen.sort_by_key(|(_, s)| Reverse(s.count));
    let sampled = seen.into_iter().map(|(name, s)| {
        let nullable = s.count < items.len() || s.types.contains_key("NULL");
        let mut types: Vec<_> = s.types.into_iter().collect();
        types.sort_by_key(|(_, count)| Reverse(*count));
        let types: Vec<_> = types.into_iter().map(|(t, _)| t).collect();
        ColumnMeta::new(name, types.join(" | "), nullable, None, None).mark_inferred()
    });
    Ok(declared.chain(sampled).collect())
}

/// The table's GSIs and LSIs as indexes, the kind in `method`.
pub(crate) fn relation_objects(rel: &RelationRef, keys: &TableKeys) -> RelationObjects {
    let indexes = keys
        .indexes
        .iter()
        .map(|(name, kind, attrs)| {
            let method = Some((*kind).to_owned());
            IndexInfo::new(
                rel.clone(),
                name,
                attrs.clone(),
                false,
                false,
                false,
                method,
            )
        })
        .collect();
    RelationObjects::new(indexes, vec![], vec![])
}

#[cfg(test)]
mod tests;
