//! The fixed columns of a DynamoDB result, chosen from the items of its first page.
//! Columns cannot grow mid-stream, so a later attribute gathers in a trailing
//! `(more attributes)` column while the result can still differ.

use std::collections::{BTreeSet, HashMap};

use aws_sdk_dynamodb::types::AttributeValue;
use serde_json::{Map, Value};

use crate::dynamo::render::{self, Cell};
use crate::value::{BatchBuilder, CellRef, ColumnInfo, DocFormat, ResultBatch};

pub(crate) const MORE_ATTRIBUTES: &str = "(more attributes)";

/// One DynamoDB item as the SDK returns it.
pub(crate) type Item = HashMap<String, AttributeValue>;

/// The attribute each column shows, and whether a gathering column follows them.
pub(crate) struct ColumnModel {
    // Looked up by name per row: a projection may repeat a name, and an attribute may itself
    // be called `(more attributes)`.
    names: Vec<String>,
    more: bool,
}

impl ColumnModel {
    /// With `projection`, exactly its names. Otherwise `keys` (hash then range), then the other
    /// attributes of `first` sorted by name, then MORE_ATTRIBUTES unless `ended`.
    pub(crate) fn of(
        first: &[Item],
        projection: Option<&[String]>,
        keys: &[String],
        ended: bool,
    ) -> (Self, Vec<ColumnInfo>) {
        let (names, more) = match projection {
            Some(names) => (names.to_vec(), false),
            None => {
                let rest: BTreeSet<&String> = first
                    .iter()
                    .flat_map(HashMap::keys)
                    .filter(|name| !keys.contains(name))
                    .collect();
                let mut names = keys.to_vec();
                names.extend(rest.into_iter().cloned());
                (names, !ended)
            }
        };
        let typed: Vec<Map<String, Value>> = first.iter().map(render::item_to_typed).collect();
        let mut columns: Vec<ColumnInfo> = names
            .iter()
            .map(|name| ColumnInfo::new(name.as_str(), shared_type(&typed, name), None))
            .collect();
        if more {
            columns.push(ColumnInfo::new(MORE_ATTRIBUTES, "M", None));
        }
        (Self { names, more }, columns)
    }

    /// Appends `item` as a row with its typed JSON document; Some(batch) when the builder closed one.
    pub(crate) fn push(&self, builder: &mut BatchBuilder, item: &Item) -> Option<ResultBatch> {
        // One conversion feeds the cells, the gathering literal and the stored document.
        let typed = render::item_to_typed(item);
        let mut cells: Vec<Cell> = self
            .names
            .iter()
            .map(|name| typed.get(name).map_or(Cell::Null, render::cell))
            .collect();
        if self.more {
            let rest: Map<String, Value> = typed
                .iter()
                .filter(|(name, _)| !self.names.contains(name))
                .map(|(name, value)| (name.clone(), value.clone()))
                .collect();
            cells.push(if rest.is_empty() {
                Cell::Null
            } else {
                Cell::Text(render::map_literal(&rest, false))
            });
        }
        let row: Vec<CellRef<'_>> = cells
            .iter()
            .map(|cell| match cell {
                Cell::Null => CellRef::Null,
                Cell::Text(s) | Cell::Number(s) => CellRef::Text(s),
                Cell::Bytes(b) => CellRef::Bytes(b),
            })
            .collect();
        let number = |col: usize| matches!(cells[col], Cell::Number(_));
        let document = Value::Object(typed).to_string();
        builder.push_document_row(&row, number, document.as_bytes())
    }
}

/// A builder whose batches hold DynamoDB documents.
pub(crate) fn builder(columns: usize) -> BatchBuilder {
    BatchBuilder::new(columns).with_document_format(DocFormat::DynamoJson)
}

/// The DynamoDB type every item that has `name` gives it, `mixed` when they differ, and empty
/// when none has it.
fn shared_type(items: &[Map<String, Value>], name: &str) -> &'static str {
    let mut seen = items
        .iter()
        .filter_map(|item| item.get(name))
        .map(render::type_name);
    match seen.next() {
        None => "",
        Some(first) if seen.all(|kind| kind == first) => first,
        Some(_) => "mixed",
    }
}

#[cfg(test)]
mod tests;
