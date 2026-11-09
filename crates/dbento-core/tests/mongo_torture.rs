//! One document per BSON type, from canonical Extended JSON through the server and back.
#![cfg(feature = "mongodb")]

use crate::support::{Cell, mg, read, text};
use bson::doc;
use dbento_core::{CellRef, Limits, ResultBatch, Session, SessionOptions};
use dbento_testkit::{
    MgUser::{Reader, Writer},
    TORTURE,
};

async fn batches(s: &mut Session, q: &str) -> Vec<ResultBatch> {
    let mut script = s.script(q);
    let p = script.next().await.unwrap().unwrap();
    let mut stream = p.execute(Limits::default()).await.unwrap();
    let mut out = Vec::new();
    while let Some(b) = stream.next_batch().await.unwrap() {
        out.push(b);
    }
    out
}

#[tokio::test]
async fn every_document_reads_back_as_its_fixture_line() {
    let fx = mg("").await;
    let mut s = fx.session(Reader, SessionOptions::default()).await;
    let got = batches(&mut s, "db.torture.find().sort({_id: 1})").await;
    let mut lines = TORTURE.lines();
    for batch in &got {
        for row in 0..batch.len() {
            let want = lines.next().expect("a row past the fixture");
            assert_eq!(batch.document(row).unwrap().to_extended_json(true), want);
        }
    }
    assert!(
        lines.next().is_none(),
        "the fixture has more lines than rows"
    );
}

#[tokio::test]
async fn every_type_has_its_cell_and_none_is_null_but_null() {
    let fx = mg("").await;
    let mut s = fx.session(Reader, SessionOptions::default()).await;
    let ran = read(&mut s, "db.torture.find().sort({_id: 1})").await;
    let names: Vec<_> = ran
        .columns
        .iter()
        .map(|(n, t)| (n.as_str(), t.as_str()))
        .collect();
    assert_eq!(names, [("_id", "int"), ("type", "string"), ("v", "mixed")]);
    let want: Vec<(&str, Cell)> = vec![
        ("double", text("1.5")),
        ("double-whole", text("2")),
        ("double-nan", text("NaN")),
        ("double-inf", text("-Infinity")),
        ("string", text("héllo")),
        ("object", text("{ b: 1, a: null }")),
        ("array", text("[ 1, 'two', null ]")),
        ("binary", text("Binary.createFromBase64('AQID', 0)")),
        ("uuid", text("UUID('01234567-89ab-cdef-0123-456789abcdef')")),
        ("binary-user", text("Binary.createFromBase64('/w==', 128)")),
        ("undefined", text(r#"{"$undefined":true}"#)),
        ("objectId", text("ObjectId('507f1f77bcf86cd799439011')")),
        ("bool", text("true")),
        ("date", text("ISODate('2024-01-02T03:04:05.123Z')")),
        (
            "date-before-1970",
            text("ISODate('1969-12-31T23:59:59.000Z')"),
        ),
        ("date-past-9999", text("new Date(253402300800000)")),
        ("null", Cell::Null),
        ("regex", text("/a.c/im")),
        (
            "dbPointer",
            text(r#"{"$dbPointer":{"$ref":"app.t","$id":{"$oid":"507f1f77bcf86cd799439011"}}}"#),
        ),
        ("code", text("Code('function () {}')")),
        ("symbol", text(r#"{"$symbol":"sym"}"#)),
        ("codeWithScope", text("Code('x', { x: 1 })")),
        ("int", text("-7")),
        ("timestamp", text("Timestamp({ t: 1704164645, i: 7 })")),
        ("long", text("9007199254740993")),
        ("decimal", text("1.10")),
        ("minKey", text("MinKey()")),
        ("maxKey", text("MaxKey()")),
    ];
    assert_eq!(ran.rows.len(), want.len());
    for (row, (kind, cell)) in ran.rows.iter().zip(want) {
        assert_eq!(row[1], text(kind));
        assert_eq!(row[2], cell, "{kind}");
    }
}

#[tokio::test]
async fn numbers_carry_the_number_bit_and_strings_do_not() {
    let fx = mg("").await;
    let mut s = fx.session(Reader, SessionOptions::default()).await;
    let got = batches(&mut s, "db.torture.find().sort({_id: 1})").await;
    let batch = &got[0];
    assert_eq!(batch.len(), TORTURE.lines().count());
    for row in 0..batch.len() {
        let CellRef::Text(kind) = batch.cell(row, 1) else {
            panic!()
        };
        let numeric = kind.starts_with("double") || ["int", "long", "decimal"].contains(&kind);
        assert_eq!(batch.is_number(row, 2), numeric, "{kind}");
    }
}

#[tokio::test]
async fn absent_and_null_are_the_same_cell_but_different_documents() {
    let fx = mg("").await;
    let mut s = fx.session(Writer, SessionOptions::default()).await;
    fx.db
        .run(doc! { "insert": "gaps", "documents": [{ "_id": 1, "a": null }, { "_id": 2 }] })
        .await;
    let ran = read(&mut s, "db.gaps.find().sort({_id: 1})").await;
    assert_eq!(ran.rows[0][1], Cell::Null);
    assert_eq!(ran.rows[1][1], Cell::Null);
    let got = batches(&mut s, "db.gaps.find().sort({_id: 1})").await;
    assert_eq!(
        got[0].document(0).unwrap().to_extended_json(false),
        r#"{"_id":1,"a":null}"#
    );
    assert_eq!(
        got[0].document(1).unwrap().to_extended_json(false),
        r#"{"_id":2}"#
    );
    assert_eq!(
        got[0].document(0).unwrap().to_shell(false),
        "{ _id: 1, a: null }"
    );
    assert_eq!(got[0].document(1).unwrap().to_shell(true), "{\n  _id: 2\n}");
}

#[tokio::test]
async fn skip_keeps_each_document_with_its_row_on_real_results() {
    let fx = mg("").await;
    let mut s = fx.session(Reader, SessionOptions::default()).await;
    let got = batches(&mut s, "db.many.find().sort({_id: 1}).limit(10)").await;
    let tail = got[0].skip(4);
    assert_eq!(tail.cell(0, 0), CellRef::Text("4"));
    assert!(
        tail.document(0)
            .unwrap()
            .to_shell(false)
            .starts_with("{ _id: 4,")
    );
}
