//! Fixes a result's columns from its first rows: a schemaless row names its own fields, so the
//! union of what is buffered becomes the header, and anything later goes in a trailing column.

use std::borrow::Cow;

use super::cell::{Cell, cell, surql_text};
use super::decode::Wire;
use crate::value::ColumnInfo;

/// The trailing column that holds fields the buffered rows did not show.
pub(crate) const MORE_FIELDS: &str = "(more fields)";
/// The column of a statement whose rows are not objects.
const VALUE: &str = "value";

/// How a result's rows turn into cells.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Shape {
    names: Vec<String>,
    /// One `value` column instead of named ones.
    value: bool,
    /// A trailing `(more fields)` column follows the names.
    more: bool,
}

impl Shape {
    /// The shape of `rows`, and its columns. `finished` means no later row can bring a field.
    pub(crate) fn of(rows: &[Wire], single: bool, finished: bool) -> (Shape, Vec<ColumnInfo>) {
        let objects = !single && rows.iter().all(|row| matches!(row, Wire::Object(_)));
        if rows.is_empty() {
            let shape = Shape {
                names: Vec::new(),
                value: false,
                more: false,
            };
            return (shape, Vec::new());
        }
        if !objects {
            let shape = Shape {
                names: Vec::new(),
                value: true,
                more: false,
            };
            let column = ColumnInfo::new(VALUE, common_type(rows.iter()), None);
            return (shape, vec![column]);
        }
        let mut names: Vec<String> = Vec::new();
        for row in rows {
            if let Wire::Object(fields) = row {
                for (name, _) in fields {
                    if !names.contains(name) {
                        names.push(name.clone());
                    }
                }
            }
        }
        // The wire sorts every map, so a union of sorted keys is sorted too, with `id` first.
        names.sort();
        if let Some(at) = names.iter().position(|name| name == "id") {
            let id = names.remove(at);
            names.insert(0, id);
        }
        let mut columns: Vec<ColumnInfo> = names
            .iter()
            .map(|name| {
                let values = rows.iter().filter_map(|row| value_of(row, name));
                ColumnInfo::new(name, common_type(values), None)
            })
            .collect();
        if !finished {
            columns.push(ColumnInfo::new(MORE_FIELDS, "object", None));
        }
        let shape = Shape {
            names,
            value: false,
            more: !finished,
        };
        (shape, columns)
    }

    /// How many cells a row of this shape holds.
    pub(crate) fn width(&self) -> usize {
        usize::from(self.value) + self.names.len() + usize::from(self.more)
    }

    /// `row` as cells: a field the header lacks goes to `(more fields)` as object text.
    pub(crate) fn cells<'w>(&self, row: &'w Wire) -> Vec<Cell<'w>> {
        if self.value {
            return vec![cell(row)];
        }
        let mut cells: Vec<Cell<'w>> = self
            .names
            .iter()
            .map(|name| value_of(row, name).map_or(Cell::Null, cell))
            .collect();
        if self.more {
            cells.push(self.extras(row));
        }
        cells
    }

    fn extras<'w>(&self, row: &'w Wire) -> Cell<'w> {
        let Wire::Object(fields) = row else {
            return Cell::Text(Cow::Owned(surql_text(row)));
        };
        let extra: Vec<(String, Wire)> = fields
            .iter()
            .filter(|(name, _)| !self.names.contains(name))
            .cloned()
            .collect();
        if extra.is_empty() {
            Cell::Null
        } else {
            Cell::Text(Cow::Owned(surql_text(&Wire::Object(extra))))
        }
    }
}

fn value_of<'w>(row: &'w Wire, name: &str) -> Option<&'w Wire> {
    super::wire::field(row, name)
}

// The one type every non-null value shares, or `mixed`; `any` when all are null.
fn common_type<'w>(values: impl Iterator<Item = &'w Wire>) -> &'static str {
    let mut common = None;
    for value in values {
        let Some(name) = type_name(value) else {
            continue;
        };
        match common {
            None => common = Some(name),
            Some(seen) if seen == name => {}
            Some(_) => return "mixed",
        }
    }
    common.unwrap_or("any")
}

fn type_name(value: &Wire) -> Option<&'static str> {
    Some(match value {
        Wire::None | Wire::Null | Wire::Undefined => return None,
        Wire::Bool(_) => "bool",
        Wire::Int(_) => "int",
        Wire::Float(_) => "float",
        Wire::Decimal(_) => "decimal",
        Wire::Text(_) => "string",
        Wire::Bytes(_) => "bytes",
        Wire::Datetime { .. } => "datetime",
        Wire::Duration { .. } => "duration",
        Wire::Uuid(_) => "uuid",
        Wire::Table(_) => "table",
        Wire::Record { .. } => "record",
        Wire::Range(_) => "range",
        Wire::Geometry { .. } => "geometry",
        Wire::Set(_) => "set",
        Wire::File { .. } => "file",
        Wire::Array(_) => "array",
        Wire::Object(_) => "object",
        Wire::Unknown { .. } => "unknown",
    })
}

#[cfg(test)]
mod tests;
