//! One result row as record fields: markers, shown values, display and edit text.
mod support;
use dbento_app::record::{
    DOCUMENT_CAP, Marker, capped_document, display_text, edit_text, parse_edit, pretty_json,
    record_at, record_fields, record_position,
};
use dbento_app::staged::GridRow::{Inserted, Loaded};
use dbento_core::EditValue;
use gpui_kit::TestAppContext;
use support::{fixture, ready, wait_real};

const T: &str = "CREATE TABLE a(id INTEGER PRIMARY KEY, name TEXT); \
                 CREATE TABLE b(id INTEGER PRIMARY KEY, a_id INTEGER REFERENCES a(id), doc JSON, \
                 pic BLOB, note TEXT); \
                 INSERT INTO a VALUES (1, 'x'); \
                 INSERT INTO b VALUES (1, 1, '{\"z\":1,\"a\":[1.50,{}]}', x'01ff', NULL), \
                 (2, NULL, 'not json', NULL, 'two\nlines');";

fn text(s: &str) -> EditValue {
    EditValue::Text(s.into())
}

#[test]
fn pretty_json_keeps_numbers_and_key_order() {
    assert_eq!(
        pretty_json("{\"z\":1, \"a\" : [1.50,{}], \"s\":\"x,{\\\"\"}").as_deref(),
        Some("{\n  \"z\": 1,\n  \"a\": [\n    1.50,\n    {}\n  ],\n  \"s\": \"x,{\\\"\"\n}")
    );
    assert_eq!(pretty_json("[]").as_deref(), Some("[]"));
    assert_eq!(pretty_json("42").as_deref(), Some("42"));
    assert_eq!(pretty_json("{not json"), None);
}

#[test]
fn display_and_edit_text_of_each_value_kind() {
    assert_eq!(display_text(&EditValue::Null, "TEXT"), "NULL");
    assert_eq!(display_text(&EditValue::Default, "TEXT"), "DEFAULT");
    assert_eq!(
        display_text(&EditValue::Bytes(vec![1, 0xff]), "BLOB"),
        "\\x01ff"
    );
    assert_eq!(
        display_text(&text("{\"a\":1}"), "jsonb"),
        "{\n  \"a\": 1\n}"
    );
    assert_eq!(display_text(&text("not json"), "JSON"), "not json");
    assert_eq!(display_text(&text("{\"a\":1}"), "text"), "{\"a\":1}");
    assert_eq!(edit_text(&EditValue::Null), "");
    assert_eq!(edit_text(&EditValue::Default), "");
    assert_eq!(edit_text(&EditValue::Bytes(vec![0xab])), "\\xab");
    assert_eq!(edit_text(&text("two\nlines")), "two\nlines");
}

#[test]
fn parse_edit_rules() {
    assert_eq!(parse_edit("", &EditValue::Null, false), Ok(None));
    assert_eq!(parse_edit("", &EditValue::Default, true), Ok(None));
    assert_eq!(parse_edit("same", &text("same"), false), Ok(None));
    assert_eq!(parse_edit("", &text("x"), false), Ok(Some(text(""))));
    assert_eq!(
        parse_edit("a\nb", &EditValue::Null, false),
        Ok(Some(text("a\nb")))
    );
    assert_eq!(
        parse_edit("\\x0A0b", &EditValue::Bytes(vec![1]), true),
        Ok(Some(EditValue::Bytes(vec![10, 11])))
    );
    assert_eq!(
        parse_edit("\\x01", &EditValue::Bytes(vec![1]), true),
        Ok(None)
    );
    assert_eq!(
        parse_edit("", &EditValue::Bytes(vec![1]), true),
        Ok(Some(EditValue::Bytes(vec![])))
    );
    let bad = "bytes are written as \\x and hex digits".to_owned();
    assert_eq!(parse_edit("zz", &EditValue::Null, true), Err(bad.clone()));
    assert_eq!(parse_edit("\\x0", &EditValue::Null, true), Err(bad));
}

#[test]
fn positions_name_loaded_and_inserted_rows() {
    assert_eq!(record_position(Loaded(2), 40), "Row 3 of 40");
    assert_eq!(record_position(Inserted(0), 40), "New row 1");
}

#[gpui_kit::test]
fn a_browse_row_carries_markers_values_and_staged_edits(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = ready(cx, &fx, "b");
    m.read_with(cx, |m, _| {
        let f = record_fields(m, 0, 0);
        let names: Vec<&str> = f.iter().map(|f| f.name.as_str()).collect();
        assert_eq!(names, ["id", "a_id", "doc", "pic", "note"]);
        assert_eq!(f[0].marker, Some(Marker::PrimaryKey));
        assert_eq!(
            (f[1].marker, f[1].references.as_deref()),
            (Some(Marker::ForeignKey), Some("a"))
        );
        assert_eq!(f[1].type_name, "INTEGER");
        assert_eq!(f[3].value, EditValue::Bytes(vec![1, 0xff]));
        assert_eq!(f[4].value, EditValue::Null);
        assert!(f.iter().all(|f| f.editable && !f.modified));
        assert_eq!(
            display_text(&f[2].value, &f[2].type_name),
            "{\n  \"z\": 1,\n  \"a\": [\n    1.50,\n    {}\n  ]\n}"
        );
        assert!(record_fields(m, 0, 9).is_empty());
    });
    assert!(m.update(cx, |m, cx| m.set_value(Loaded(0), 4, text("n"), cx)));
    m.read_with(cx, |m, _| {
        let f = record_fields(m, 0, 0);
        assert_eq!((f[4].value.clone(), f[4].modified), (text("n"), true));
    });
    assert!(m.update(cx, |m, cx| m.delete_at(Loaded(1), cx)));
    m.read_with(cx, |m, _| {
        assert!(record_fields(m, 0, 1).iter().all(|f| !f.editable))
    });
}

#[gpui_kit::test]
fn an_inserted_row_reads_as_default_and_new(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = ready(cx, &fx, "b");
    assert_eq!(m.update(cx, |m, cx| m.add_row(cx)), Some(0));
    m.read_with(cx, |m, _| {
        assert_eq!(record_at(m, 0, 0), Inserted(0));
        assert_eq!(record_at(m, 0, 1), Loaded(0));
        assert_eq!(record_position(record_at(m, 0, 0), 2), "New row 1");
        let f = record_fields(m, 0, 0);
        assert_eq!(f[1].value, EditValue::Default);
        assert!(f[1].editable && !f[1].modified);
        assert_eq!(record_fields(m, 0, 1)[0].value, text("1"));
    });
}

#[gpui_kit::test]
fn a_free_result_has_no_markers_and_is_read_only(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = ready(cx, &fx, "b");
    m.update(cx, |m, cx| {
        m.run("SELECT id, a_id FROM b ORDER BY id".into(), cx)
    });
    wait_real(cx, "the free result", |cx| {
        m.read_with(cx, |m, _| {
            !m.running() && !m.browse_result() && !m.results().is_empty()
        })
    });
    m.read_with(cx, |m, _| {
        let f = record_fields(m, 0, 1);
        assert_eq!(f[0].value, text("2"));
        assert_eq!(f[1].value, EditValue::Null);
        assert!(f.iter().all(|f| f.marker.is_none() && !f.editable));
        assert_eq!(record_at(m, 0, 1), Loaded(1));
    });
}

#[test]
fn unchanged_text_in_a_bytes_field_stages_nothing() {
    assert_eq!(parse_edit("hello", &text("hello"), true), Ok(None));
}

#[test]
fn a_sign_is_not_a_hex_digit() {
    let bad = "bytes are written as \\x and hex digits".to_owned();
    assert_eq!(
        parse_edit("\\x+f", &EditValue::Null, true),
        Err(bad.clone())
    );
    assert_eq!(parse_edit("\\x-1", &EditValue::Null, true), Err(bad));
}

#[test]
fn a_document_past_the_cap_is_cut_on_a_char_boundary_with_a_note() {
    let short = "{ a: 1 }".to_owned();
    assert_eq!(capped_document(short.clone(), DOCUMENT_CAP), short);
    // A two-byte char straddles the cap, so the cut must step back to its start.
    let long = format!("{}é{}", "x".repeat(9), "y".repeat(100));
    let cut = capped_document(long, 10);
    assert!(cut.starts_with("xxxxxxxxx\n"), "{cut}");
    assert!(cut.contains("Copy as JSON"), "{cut}");
    let big = "z".repeat(DOCUMENT_CAP * 4);
    assert!(capped_document(big, DOCUMENT_CAP).len() < DOCUMENT_CAP + 200);
}
