//! The fixed columns of a MongoDB result, chosen from the documents buffered before its first row.
//! Columns cannot grow mid-stream, so a later field gathers in a trailing `(more fields)` column.

use std::collections::HashMap;
use std::collections::hash_map::Entry;

use bson::{Bson, Document, RawDocument, RawDocumentBuf};

use crate::mongo::render::{self, CellText};
use crate::session::ExecuteError;
use crate::value::{BatchBuilder, CellRef, ColumnInfo, ResultBatch};

pub(crate) const MORE_FIELDS: &str = "(more fields)";

/// Where each field of a result goes; `more` is the `(more fields)` column when it exists.
pub(crate) struct ColumnModel {
    index: HashMap<String, usize>,
    more: Option<usize>,
}

impl ColumnModel {
    /// `buffer`'s columns; `ended` says the cursor finished inside it.
    pub(crate) fn of(
        buffer: &[RawDocumentBuf],
        ended: bool,
    ) -> Result<(Self, Vec<ColumnInfo>), ExecuteError> {
        let mut fields: Vec<(String, &'static str)> = Vec::new();
        let mut slots: HashMap<String, usize> = HashMap::new();
        for raw in buffer {
            for (name, value) in decode(raw)? {
                let seen = render::type_name(&value);
                match slots.entry(name) {
                    Entry::Occupied(slot) => {
                        let kind = &mut fields[*slot.get()].1;
                        if *kind != seen {
                            *kind = "mixed";
                        }
                    }
                    Entry::Vacant(slot) => {
                        fields.push((slot.key().clone(), seen));
                        slot.insert(fields.len() - 1);
                    }
                }
            }
        }
        if let Some(&id) = slots.get("_id") {
            fields[..=id].rotate_right(1);
        }
        // A grid needs a column even with no field to name one, and the empty type tells a
        // shell that this is a read with nothing to show, not a write.
        if fields.is_empty() {
            fields.push(("_id".to_owned(), ""));
        }
        let index = fields
            .iter()
            .enumerate()
            .map(|(col, (name, _))| (name.clone(), col))
            .collect();
        let more = (!ended).then_some(fields.len());
        let mut columns: Vec<ColumnInfo> = fields
            .into_iter()
            .map(|(name, kind)| ColumnInfo::new(name, kind, None))
            .collect();
        if more.is_some() {
            columns.push(ColumnInfo::new(MORE_FIELDS, "object", None));
        }
        Ok((Self { index, more }, columns))
    }

    /// Appends `raw` as a row with its document.
    pub(crate) fn push(
        &self,
        builder: &mut BatchBuilder,
        raw: &RawDocument,
    ) -> Result<Option<ResultBatch>, ExecuteError> {
        let width = self.index.len() + usize::from(self.more.is_some());
        let mut cells = vec![CellText::Null; width];
        let mut rest = Document::new();
        for (name, value) in decode(raw)? {
            match self.index.get(&name) {
                Some(&col) => cells[col] = render::cell(&value),
                None => {
                    rest.insert(name, value);
                }
            }
        }
        if let Some(col) = self.more
            && !rest.is_empty()
        {
            cells[col] = CellText::Text(render::shell(&Bson::Document(rest)));
        }
        let row: Vec<CellRef<'_>> = cells
            .iter()
            .map(|cell| match cell {
                CellText::Null => CellRef::Null,
                CellText::Text(s) | CellText::Number(s) => CellRef::Text(s),
            })
            .collect();
        let number = |col: usize| matches!(cells[col], CellText::Number(_));
        Ok(builder.push_document_row(&row, number, raw.as_bytes()))
    }
}

pub(super) fn decode(raw: &RawDocument) -> Result<Document, ExecuteError> {
    render::decode(raw).map_err(|reason| ExecuteError::database("driver", reason))
}

#[cfg(test)]
mod tests;
