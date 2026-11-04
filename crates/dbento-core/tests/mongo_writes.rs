//! Writes through a session: ids, counts, timeouts and the errors of an ok reply.
#![cfg(feature = "mongodb")]
mod support;

use std::time::Duration;

use bson::{Document, RawDocumentBuf, doc};
use dbento_core::{ExecuteError, Limits, Session, SessionOptions};
use dbento_testkit::{
    MgDb,
    MgUser::{Reader, Root, Writer},
};
use support::{code_of, failure, mg, one, read, run};

async fn affected(s: &mut Session, text: &str) -> Option<u64> {
    let ran = one(s, text, Limits::default()).await;
    assert!(ran.columns.is_empty(), "{text}");
    ran.outcome.rows_affected
}

async fn all(db: &MgDb, collection: &str) -> Vec<Document> {
    let reply = db
        .run(doc! { "find": collection, "sort": { "_id": 1 } })
        .await;
    let batch = reply
        .get_document("cursor")
        .unwrap()
        .get_array("firstBatch")
        .unwrap();
    batch
        .iter()
        .map(|d| d.as_document().unwrap().clone())
        .collect()
}

fn timed(ms: u64) -> SessionOptions {
    let mut o = SessionOptions::default();
    o.statement_timeout = Some(Duration::from_millis(ms));
    o
}

#[tokio::test]
async fn an_insert_assigns_an_id_and_counts_its_documents() {
    let fx = mg("").await;
    let mut s = fx.session(Writer, SessionOptions::default()).await;
    assert_eq!(affected(&mut s, "db.w.insertOne({a: 1})").await, Some(1));
    assert_eq!(
        affected(&mut s, "db.w.insertMany([{a: 2}, {a: 3}])").await,
        Some(2)
    );
    let docs = all(&fx.db, "w").await;
    assert_eq!(docs.len(), 3);
    assert!(docs.iter().all(|d| d.get_object_id("_id").is_ok()));
}

#[tokio::test]
async fn updates_count_modified_plus_upserted_and_deletes_count_n() {
    let fx = mg("").await;
    let mut s = fx.session(Writer, SessionOptions::default()).await;
    affected(
        &mut s,
        "db.w.insertMany([{_id: 1, a: 1}, {_id: 2, a: 1}, {_id: 3, a: 1}])",
    )
    .await;
    assert_eq!(
        affected(&mut s, "db.w.updateMany({}, {$set: {a: 2}})").await,
        Some(3)
    );
    assert_eq!(
        affected(&mut s, "db.w.updateMany({}, {$set: {a: 2}})").await,
        Some(0)
    );
    assert_eq!(
        affected(
            &mut s,
            "db.w.updateOne({_id: 9}, {$set: {a: 9}}, {upsert: true})"
        )
        .await,
        Some(1)
    );
    assert_eq!(
        affected(&mut s, "db.w.replaceOne({_id: 1}, {b: 1})").await,
        Some(1)
    );
    assert_eq!(affected(&mut s, "db.w.deleteOne({a: 2})").await, Some(1));
    assert_eq!(affected(&mut s, "db.w.deleteMany({})").await, Some(3));
}

#[tokio::test]
async fn array_filters_reach_the_server_in_the_statement() {
    let fx = mg("").await;
    let mut s = fx.session(Writer, SessionOptions::default()).await;
    affected(&mut s, "db.w.insertOne({_id: 1, g: [1, 5, 9]})").await;
    let update = "db.w.updateOne({_id: 1}, {$set: {'g.$[e]': 0}}, {arrayFilters: [{e: {$gt: 4}}]})";
    assert_eq!(affected(&mut s, update).await, Some(1));
    assert_eq!(all(&fx.db, "w").await[0], doc! { "_id": 1, "g": [1, 0, 0] });
}

#[tokio::test]
async fn ddl_methods_report_no_rows() {
    let fx = mg("").await;
    let mut s = fx.session(Writer, SessionOptions::default()).await;
    assert_eq!(
        affected(&mut s, "db.createCollection('made')").await,
        Some(0)
    );
    assert_eq!(
        affected(&mut s, "db.made.createIndex({a: 1})").await,
        Some(0)
    );
    assert_eq!(affected(&mut s, "db.made.dropIndex('a_1')").await, Some(0));
    assert_eq!(affected(&mut s, "db.made.drop()").await, Some(0));
}

#[tokio::test]
async fn a_statement_can_name_its_database_and_use_switches_it() {
    let fx = mg("").await;
    let mut s = fx.session(Root, SessionOptions::default()).await;
    affected(
        &mut s,
        "db.getSiblingDB('elsewhere_w').w.insertOne({_id: 1})",
    )
    .await;
    read(&mut s, "use elsewhere_w").await;
    assert_eq!(affected(&mut s, "db.w.insertOne({_id: 2})").await, Some(1));
    assert_eq!(fx.db.count("w").await, 0);
    let elsewhere = fx.db.client(Root).await.database("elsewhere_w");
    let n = elsewhere.run_command(doc! { "count": "w" }).await.unwrap();
    assert_eq!(n.get_i32("n").unwrap(), 2);
    elsewhere
        .run_command(doc! { "dropDatabase": 1 })
        .await
        .unwrap();
}

#[tokio::test]
async fn a_slow_write_stops_at_the_statement_timeout() {
    let fx = mg("").await;
    let mut s = fx.session(Writer, timed(50)).await;
    // 10,000 documents at 10 ms each need 100 s, so 50 ms always fires first.
    let update = "db.many.updateMany({$where: 'sleep(10) || true'}, {$set: {z: 1}})";
    let delete = "db.many.deleteMany({$where: 'sleep(10) || true'})";
    for statement in [update, delete] {
        assert!(matches!(
            failure(&mut s, statement).await,
            ExecuteError::Timeout
        ));
    }
}

#[tokio::test]
async fn a_failed_update_many_says_it_may_have_changed_documents() {
    let fx = mg("").await;
    fx.db
        .run(doc! { "insert": "w", "documents": [{ "_id": 1, "a": 1 }, { "_id": 2, "a": 2 }] })
        .await;
    fx.db
        .run(doc! {
            "createIndexes": "w",
            "indexes": [{ "key": { "a": 1 }, "name": "a_1", "unique": true }],
        })
        .await;
    let mut s = fx.session(Writer, SessionOptions::default()).await;
    let update = "db.w.updateMany({}, {$set: {a: 5}})";
    match run(&mut s, update, Limits::default()).await.remove(0) {
        Err(ExecuteError::Database { code, detail, .. }) => {
            assert_eq!(code, "11000");
            assert_eq!(
                detail.as_deref(),
                Some("the update may have changed some documents before it failed")
            );
        }
        other => panic!("{:?}", other.map(|r| r.outcome)),
    }
    assert_eq!(all(&fx.db, "w").await[0], doc! { "_id": 1, "a": 5 });
}

// The write lands, and only then does the driver fail to decode the reply holding the document.
#[tokio::test]
async fn an_unreadable_reply_to_a_write_says_it_may_have_landed() {
    let fx = mg("").await;
    // {_id: 1, a: "\xff"}: length, int `_id`, string `a` of length 2, the byte, NUL, end.
    let bytes = [
        23, 0, 0, 0, 0x10, b'_', b'i', b'd', 0, 1, 0, 0, 0, 2, b'a', 0, 2, 0, 0, 0, 0xff, 0, 0,
    ];
    let stored = RawDocumentBuf::from_bytes(bytes.to_vec()).unwrap();
    fx.db
        .client(Root)
        .await
        .database(fx.db.database())
        .collection::<RawDocumentBuf>("bad")
        .insert_one(stored)
        .await
        .unwrap();
    let mut s = fx.session(Writer, SessionOptions::default()).await;
    let update = "db.bad.findOneAndUpdate({_id: 1}, {$set: {b: 1}})";
    match run(&mut s, update, Limits::default()).await.remove(0) {
        Err(ExecuteError::Database { code, detail, .. }) => {
            assert_eq!(code, "driver");
            assert_eq!(
                detail.as_deref(),
                Some("the write may have landed, but its reply could not be read")
            );
        }
        other => panic!("{:?}", other.map(|r| r.outcome)),
    }
    let landed = fx
        .db
        .run(doc! { "count": "bad", "query": { "b": 1 } })
        .await;
    assert_eq!(landed.get_i32("n").unwrap(), 1);
}

#[tokio::test]
async fn a_duplicate_key_fails_and_says_what_was_written_first() {
    let fx = mg("").await;
    let mut s = fx.session(Writer, SessionOptions::default()).await;
    let many = "db.w.insertMany([{_id: 1}, {_id: 2}, {_id: 1}, {_id: 3}])";
    match run(&mut s, many, Limits::default()).await.remove(0) {
        Err(ExecuteError::Database {
            code,
            message,
            detail,
            ..
        }) => {
            assert_eq!(code, "11000");
            assert!(message.contains("E11000"), "{message}");
            assert_eq!(
                detail.as_deref(),
                Some("the server reported 2 documents written, which may be inexact")
            );
        }
        other => panic!("{:?}", other.map(|r| r.outcome)),
    }
    assert_eq!(fx.db.count("w").await, 2);
}

#[tokio::test]
async fn a_server_refusal_carries_its_code_name() {
    let fx = mg("").await;
    let mut s = fx.session(Writer, SessionOptions::default()).await;
    assert_eq!(
        code_of(failure(&mut s, "db.many.dropIndex('nope')").await),
        "IndexNotFound"
    );
}

// Outside a read-only session nothing is a read-only violation, so the server's word stands.
#[tokio::test]
async fn a_reader_outside_read_only_gets_the_servers_refusal() {
    let fx = mg("").await;
    let mut s = fx.session(Reader, SessionOptions::default()).await;
    assert_eq!(
        code_of(failure(&mut s, "db.many.insertOne({a: 1})").await),
        "Unauthorized"
    );
    assert_eq!(fx.db.count("many").await, 10_000);
}
