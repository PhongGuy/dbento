use aws_sdk_dynamodb::primitives::Blob;
use aws_sdk_dynamodb::types::AttributeValue;

use super::{ColumnModel, Item, MORE_ATTRIBUTES, builder};
use crate::value::{CellRef, ColumnInfo, ResultBatch};

fn item(pairs: &[(&str, AttributeValue)]) -> Item {
    pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect()
}

fn s(x: &str) -> AttributeValue {
    AttributeValue::S(x.to_owned())
}

fn n(x: &str) -> AttributeValue {
    AttributeValue::N(x.to_owned())
}

fn keys(k: &[&str]) -> Vec<String> {
    k.iter().map(|x| (*x).to_owned()).collect()
}

fn described(columns: &[ColumnInfo]) -> Vec<(&str, &str)> {
    columns.iter().map(|c| (c.name(), c.type_name())).collect()
}

fn batch_of(model: &ColumnModel, width: usize, items: &[Item]) -> ResultBatch {
    let mut b = builder(width);
    for i in items {
        assert!(
            model.push(&mut b, i).is_none(),
            "a small batch never closes"
        );
    }
    b.finish()
}

#[test]
fn key_attributes_come_first_then_the_rest_sorted_by_name() {
    let first = [
        item(&[
            ("status", s("open")),
            ("sk", n("1")),
            ("pk", s("c1")),
            ("amount", n("5")),
        ]),
        item(&[("pk", s("c2")), ("sk", n("2")), ("note", s("x"))]),
    ];
    let (_, columns) = ColumnModel::of(&first, None, &keys(&["pk", "sk"]), true);
    assert_eq!(
        described(&columns),
        [
            ("pk", "S"),
            ("sk", "N"),
            ("amount", "N"),
            ("note", "S"),
            ("status", "S")
        ]
    );
}

#[test]
fn a_projection_is_the_columns_exactly_with_nothing_added() {
    let first = [item(&[("status", s("open"))])];
    let projection = keys(&["status", "missing"]);
    let (model, columns) = ColumnModel::of(&first, Some(&projection), &keys(&["pk", "sk"]), false);
    assert_eq!(described(&columns), [("status", "S"), ("missing", "")]);
    let batch = batch_of(&model, columns.len(), &first);
    assert_eq!(
        (batch.cell(0, 0), batch.cell(0, 1)),
        (CellRef::Text("open"), CellRef::Null)
    );
}

#[test]
fn a_column_of_several_types_is_mixed_and_absent_attributes_do_not_count() {
    let first = [
        item(&[("a", n("1")), ("b", s("x"))]),
        item(&[("a", s("1"))]),
        item(&[("b", AttributeValue::Null(true))]),
    ];
    let (_, columns) = ColumnModel::of(&first, None, &[], true);
    assert_eq!(described(&columns), [("a", "mixed"), ("b", "mixed")]);
}

#[test]
fn more_attributes_exists_only_when_the_result_can_still_differ() {
    let first = [item(&[("pk", s("a"))])];
    let (_, ended) = ColumnModel::of(&first, None, &keys(&["pk"]), true);
    assert_eq!(described(&ended), [("pk", "S")]);
    let (_, open) = ColumnModel::of(&first, None, &keys(&["pk"]), false);
    assert_eq!(described(&open), [("pk", "S"), (MORE_ATTRIBUTES, "M")]);
    let (_, empty) = ColumnModel::of(&[], None, &keys(&["pk", "sk"]), true);
    assert_eq!(described(&empty), [("pk", ""), ("sk", "")]);
    let (_, nothing) = ColumnModel::of(&[], None, &[], true);
    assert!(nothing.is_empty(), "{nothing:?}");
}

#[test]
fn a_row_fills_its_columns_marks_numbers_and_gathers_later_attributes() {
    let first = [item(&[("pk", s("a")), ("n", n("1.50"))])];
    let (model, columns) = ColumnModel::of(&first, None, &keys(&["pk"]), false);
    let later = item(&[
        ("pk", s("b")),
        ("z", AttributeValue::Ss(vec!["x".to_owned()])),
        ("bin", AttributeValue::B(Blob::new(vec![0u8, 255]))),
    ]);
    let batch = batch_of(&model, columns.len(), &[first[0].clone(), later]);
    assert_eq!(batch.cell(0, 1), CellRef::Text("1.50"));
    assert!(
        batch.is_number(0, 1) && !batch.is_number(0, 0),
        "only N is a number"
    );
    assert_eq!(batch.cell(0, 2), CellRef::Null);
    assert_eq!(batch.cell(1, 1), CellRef::Null);
    assert_eq!(
        batch.cell(1, 2),
        CellRef::Text("{'bin': 'AP8=', 'z': <<'x'>>}")
    );
}

#[test]
fn binary_cells_are_bytes_and_null_differs_from_absent_only_in_the_document() {
    let first = [
        item(&[
            ("pk", s("a")),
            ("b", AttributeValue::B(Blob::new(vec![1u8, 2]))),
            ("v", AttributeValue::Null(true)),
        ]),
        item(&[("pk", s("b"))]),
    ];
    let (model, columns) = ColumnModel::of(&first, None, &keys(&["pk"]), true);
    let batch = batch_of(&model, columns.len(), &first);
    assert_eq!(batch.cell(0, 1), CellRef::Bytes(&[1, 2]));
    assert_eq!(
        (batch.cell(0, 2), batch.cell(1, 2)),
        (CellRef::Null, CellRef::Null)
    );
    let doc = |row: usize| batch.document(row).unwrap().to_extended_json(true);
    assert_eq!(
        doc(0),
        r#"{"b":{"B":"AQI="},"pk":{"S":"a"},"v":{"NULL":true}}"#
    );
    assert_eq!(doc(1), r#"{"pk":{"S":"b"}}"#);
}

#[test]
fn each_row_keeps_its_item_as_typed_json_and_renders_it_three_ways() {
    let big = "12345678901234567890123456789012345678";
    let first = [item(&[
        ("pk", s("c1")),
        ("n", n(big)),
        (
            "tags",
            AttributeValue::Ss(vec!["a".to_owned(), "b".to_owned()]),
        ),
    ])];
    let (model, columns) = ColumnModel::of(&first, None, &keys(&["pk"]), true);
    let batch = batch_of(&model, columns.len(), &first);
    let doc = batch.document(0).unwrap();
    assert_eq!(
        doc.to_extended_json(true),
        format!(r#"{{"n":{{"N":"{big}"}},"pk":{{"S":"c1"}},"tags":{{"SS":["a","b"]}}}}"#)
    );
    assert_eq!(
        doc.to_extended_json(false),
        format!(r#"{{"n":{big},"pk":"c1","tags":["a","b"]}}"#)
    );
    assert_eq!(
        doc.to_shell(false),
        format!("{{'n': {big}, 'pk': 'c1', 'tags': <<'a', 'b'>>}}")
    );
    assert_eq!(
        doc.to_shell(true),
        format!("{{\n  'n': {big},\n  'pk': 'c1',\n  'tags': <<'a', 'b'>>\n}}")
    );
}

#[test]
fn documents_stay_aligned_and_keep_their_format_through_skip() {
    let rows: Vec<Item> = ["a", "b", "c"]
        .iter()
        .map(|k| item(&[("pk", s(k))]))
        .collect();
    let (model, columns) = ColumnModel::of(&rows, None, &keys(&["pk"]), true);
    let tail = batch_of(&model, columns.len(), &rows).skip(1);
    assert_eq!(tail.len(), 2);
    assert_eq!(tail.document(0).unwrap().to_shell(false), "{'pk': 'b'}");
    assert_eq!(
        tail.document(1).unwrap().to_extended_json(false),
        r#"{"pk":"c"}"#
    );
}

#[test]
fn document_bytes_count_toward_the_builder_total() {
    let first = [item(&[("pk", s("a"))])];
    let (model, columns) = ColumnModel::of(&first, None, &keys(&["pk"]), true);
    let mut b = builder(columns.len());
    assert!(model.push(&mut b, &first[0]).is_none(), "one row fits");
    assert_eq!(b.total_bytes(), 1 + r#"{"pk":{"S":"a"}}"#.len() as u64);
}
