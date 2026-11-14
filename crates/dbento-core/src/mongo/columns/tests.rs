use bson::{Document, RawDocumentBuf, doc};

use super::*;
use crate::value::{BatchBuilder, CellRef};

fn raws(docs: &[Document]) -> Vec<RawDocumentBuf> {
    docs.iter()
        .map(|d| RawDocumentBuf::try_from(d).unwrap())
        .collect()
}

fn described(columns: &[ColumnInfo]) -> Vec<(&str, &str)> {
    columns.iter().map(|c| (c.name(), c.type_name())).collect()
}

#[test]
fn id_comes_first_then_fields_in_the_order_seen() {
    let buffer = raws(&[
        doc! { "b": 1, "_id": 1 },
        doc! { "_id": 2, "a": "x", "b": 2 },
    ]);
    let (_, columns) = ColumnModel::of(&buffer, true).unwrap();
    assert_eq!(
        described(&columns),
        [("_id", "int"), ("b", "int"), ("a", "string")]
    );
}

#[test]
fn a_column_of_several_types_is_mixed_and_absent_fields_do_not_count() {
    let buffer = raws(&[
        doc! { "a": 1, "b": 1 },
        doc! { "a": "x" },
        doc! { "b": bson::Bson::Null },
    ]);
    let (_, columns) = ColumnModel::of(&buffer, true).unwrap();
    assert_eq!(described(&columns), [("a", "mixed"), ("b", "mixed")]);
}

#[test]
fn more_fields_exists_only_when_the_cursor_can_still_differ() {
    let buffer = raws(&[doc! { "_id": 1 }]);
    let (_, ended) = ColumnModel::of(&buffer, true).unwrap();
    assert_eq!(ended.len(), 1);
    let (_, open) = ColumnModel::of(&buffer, false).unwrap();
    assert_eq!(described(&open), [("_id", "int"), (MORE_FIELDS, "object")]);
}

#[test]
fn no_documents_is_one_id_column_with_no_type() {
    let (_, columns) = ColumnModel::of(&[], true).unwrap();
    assert_eq!(described(&columns), [("_id", "")]);
}

#[test]
fn documents_with_no_fields_still_fill_a_row() {
    let buffer = raws(&[doc! {}]);
    let (model, columns) = ColumnModel::of(&buffer, true).unwrap();
    assert_eq!(described(&columns), [("_id", "")]);
    let mut builder = BatchBuilder::new(columns.len());
    assert!(model.push(&mut builder, &buffer[0]).unwrap().is_none());
    assert_eq!(builder.finish().cell(0, 0), CellRef::Null);
}

#[test]
fn a_row_fills_its_columns_marks_numbers_and_gathers_the_rest() {
    let buffer = raws(&[doc! { "_id": 1, "a": "x" }]);
    let (model, _) = ColumnModel::of(&buffer, false).unwrap();
    let mut builder = BatchBuilder::new(3);
    let later = raws(&[doc! { "_id": 2, "z": 9, "y": [1] }]);
    for raw in buffer.iter().chain(&later) {
        assert!(model.push(&mut builder, raw).unwrap().is_none());
    }
    let batch = builder.finish();
    assert_eq!(batch.cell(0, 0), CellRef::Text("1"));
    assert!(batch.is_number(0, 0));
    assert_eq!(batch.cell(0, 1), CellRef::Text("x"));
    assert!(!batch.is_number(0, 1));
    assert_eq!(batch.cell(0, 2), CellRef::Null);
    assert_eq!(batch.cell(1, 1), CellRef::Null);
    assert_eq!(batch.cell(1, 2), CellRef::Text("{ z: 9, y: [ 1 ] }"));
    let back = batch.document(1).unwrap().to_extended_json(false);
    assert_eq!(back, r#"{"_id":2,"z":9,"y":[1]}"#);
}

#[test]
fn a_field_named_like_the_gathering_column_keeps_its_own_cell() {
    let buffer = raws(&[doc! { MORE_FIELDS: 1 }]);
    let (model, columns) = ColumnModel::of(&buffer, false).unwrap();
    assert_eq!(
        described(&columns),
        [(MORE_FIELDS, "int"), (MORE_FIELDS, "object")]
    );
    let later = raws(&[doc! { MORE_FIELDS: 2, "z": 9 }]);
    let mut builder = BatchBuilder::new(2);
    assert!(model.push(&mut builder, &later[0]).unwrap().is_none());
    let batch = builder.finish();
    assert_eq!(batch.cell(0, 0), CellRef::Text("2"));
    assert_eq!(batch.cell(0, 1), CellRef::Text("{ z: 9 }"));
    // Only the gathering column says it gathers; the real field of that name is a field.
    let gathers: Vec<bool> = columns.iter().map(ColumnInfo::is_overflow).collect();
    assert_eq!(gathers, [false, true]);
}

#[test]
fn a_rows_document_names_its_fields_in_order() {
    let buffer = raws(&[doc! { "_id": 1, "z": 9, "a": { "b": 1 } }]);
    let (model, columns) = ColumnModel::of(&buffer, true).unwrap();
    let mut builder = BatchBuilder::new(columns.len());
    assert!(model.push(&mut builder, &buffer[0]).unwrap().is_none());
    let batch = builder.finish();
    assert_eq!(batch.document(0).unwrap().field_names(), ["_id", "z", "a"]);
}

#[test]
fn an_undecodable_document_is_an_error_not_a_blank_row() {
    // A sound frame around an element type BSON lacks.
    let bytes = [8, 0, 0, 0, 0x42, b'a', 0, 0];
    let raw = bson::RawDocument::from_bytes(&bytes).unwrap();
    let buffer = [raw.to_owned()];
    assert!(matches!(
        ColumnModel::of(&buffer, true),
        Err(ExecuteError::Database { code, .. }) if code == "driver"
    ));
    let (model, columns) = ColumnModel::of(&[], false).unwrap();
    let mut builder = BatchBuilder::new(columns.len());
    assert!(matches!(
        model.push(&mut builder, raw),
        Err(ExecuteError::Database { code, .. }) if code == "driver"
    ));
    assert!(builder.is_empty());
}
