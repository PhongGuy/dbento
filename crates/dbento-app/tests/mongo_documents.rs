//! MongoDB results in the app: the store hands out each shown row's document, and Copy as
//! JSON writes the documents themselves.
#![cfg(feature = "mongodb")]
use dbento_app::copy::{CopyFormat, copy_rows};
use dbento_app::results::ResultStore;
use dbento_core::{Secret, SessionOptions, Workspace};
use dbento_testkit::{MgDb, MgUser, mongo_db};
use serde_json::{Value, json};

/// Runs each statement of `text` as the writer; the last statement's result fills the store.
async fn store(db: &MgDb, text: &str) -> ResultStore {
    let dir = tempfile::tempdir().unwrap();
    let toml = format!(
        "dbento = 1\n{}",
        db.connection_toml("mg", MgUser::Writer, "")
    );
    std::fs::write(dir.path().join("dbento.toml"), toml).unwrap();
    let ws = Workspace::open(dir.path()).unwrap();
    let conn = ws.declared("mg").unwrap();
    let secret = Secret::new(db.password(MgUser::Writer).unwrap());
    let mut s = dbento_core::connect(&ws, conn, Some(secret), SessionOptions::default())
        .await
        .unwrap();
    let mut last = None;
    {
        let mut script = s.script(text);
        while let Some(p) = script.next().await {
            let mut stream = p.unwrap().execute(Default::default()).await.unwrap();
            let mut store = ResultStore::new(stream.columns().into());
            while let Some(b) = stream.next_batch().await.unwrap() {
                store.push(b);
            }
            last = Some(store);
        }
    }
    s.close().await.unwrap();
    last.unwrap()
}

fn doc(s: &ResultStore, row: u64) -> Option<Value> {
    s.document(row)
        .map(|d| serde_json::from_str(&d.to_extended_json(false)).unwrap())
}

#[tokio::test]
async fn a_rows_document_keeps_absent_and_null_apart_and_follows_a_local_sort() {
    let db = mongo_db().await;
    let text =
        "db.shapes.insertMany([{_id: 1, a: null}, {_id: 2}]);\ndb.shapes.find().sort({_id: 1})";
    let mut s = store(&db, text).await;
    assert_eq!(s.rows(), 2);
    assert_eq!(doc(&s, 0), Some(json!({"_id": 1, "a": null})));
    assert_eq!(doc(&s, 1), Some(json!({"_id": 2})));
    assert!(s.document(0).unwrap().to_shell(true).contains("null"));
    s.sort_local(0, true);
    assert_eq!(doc(&s, 0), Some(json!({"_id": 2})));
    assert!(s.document(2).is_none());
}

#[tokio::test]
async fn copy_as_json_writes_whole_documents_whatever_columns_are_picked() {
    let db = mongo_db().await;
    let s = store(&db, "db.many.find({_id: {$lt: 2}}).sort({_id: 1})").await;
    let copied = copy_rows(&s, 0..2, &[2], &CopyFormat::Json, false);
    let v: Value = serde_json::from_str(&copied).unwrap();
    assert_eq!(
        v,
        json!([{"_id": 0, "n": 0, "s": "row 0"}, {"_id": 1, "n": 1, "s": "row 1"}])
    );
    assert_eq!(
        copy_rows(&s, 0..2, &[2], &CopyFormat::Tsv, false),
        "row 0\nrow 1"
    );
}
