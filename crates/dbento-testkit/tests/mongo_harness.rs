//! The MongoDB harness: a capped, seeded server with a reader and a writer.
#![cfg(feature = "mongodb")]
use std::time::Duration;

use bson::{Document, doc};
use dbento_testkit::{MgUser, TORTURE, mongo_db};
use mongodb::error::ErrorKind;

fn code(e: mongodb::error::Error) -> i32 {
    #[allow(
        clippy::wildcard_enum_match_arm,
        reason = "ErrorKind has too many variants to list; the rest share one arm"
    )]
    match *e.kind {
        ErrorKind::Command(c) => c.code,
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn a_database_is_seeded_and_answers_as_root() {
    let db = mongo_db().await;
    assert_eq!(db.count("many").await, 10_000);
    assert_eq!(db.count("torture").await, TORTURE.lines().count() as u64);
    let info = db.run(doc! { "buildInfo": 1 }).await;
    assert!(
        info.get_str("version").unwrap().starts_with("8.0."),
        "{info:?}"
    );
}

#[tokio::test]
async fn the_reader_only_reads_and_the_writer_only_touches_its_database() {
    let db = mongo_db().await;
    let reader = db.client(MgUser::Reader).await;
    let app = reader.database(db.database());
    let one = doc! { "find": "many", "limit": 1, "singleBatch": true };
    app.run_command(one).await.unwrap();
    let insert = doc! { "insert": "many", "documents": [{ "x": 1 }] };
    assert_eq!(code(app.run_command(insert.clone()).await.unwrap_err()), 13);

    let writer = db.client(MgUser::Writer).await;
    writer
        .database(db.database())
        .run_command(insert.clone())
        .await
        .unwrap();
    let other = writer.database("elsewhere").run_command(insert).await;
    assert_eq!(code(other.unwrap_err()), 13);

    let anonymous = db.client(MgUser::Anonymous).await;
    anonymous
        .database(db.database())
        .run_command(doc! { "ping": 1 })
        .await
        .unwrap();
}

#[tokio::test]
async fn a_container_is_capped_and_its_cache_is_small() {
    if std::env::var("DBENTO_MONGO_URL").is_ok_and(|u| !u.is_empty()) {
        return;
    }
    let db = mongo_db().await;
    assert_eq!(db.data_filesystem().await.as_deref(), Some("tmpfs"));
    let host = db.run(doc! { "hostInfo": 1 }).await;
    let limit = host
        .get_document("system")
        .unwrap()
        .get("memLimitMB")
        .unwrap();
    assert_eq!(
        limit.as_i64().or(limit.as_i32().map(i64::from)),
        Some(1024),
        "{host:?}"
    );
    let admin = db.client(MgUser::Root).await.database("admin");
    let opts: Document = admin
        .run_command(doc! { "getCmdLineOpts": 1 })
        .await
        .unwrap();
    let cache = opts
        .get_document("parsed")
        .unwrap()
        .get_document("storage")
        .unwrap()
        .get_document("wiredTiger")
        .unwrap()
        .get_document("engineConfig")
        .unwrap()
        .get_f64("cacheSizeGB")
        .unwrap();
    assert_eq!(cache, 0.25);
}

#[tokio::test]
async fn a_third_container_waits_for_a_slot() {
    if std::env::var("DBENTO_MONGO_URL").is_ok_and(|u| !u.is_empty()) {
        return;
    }
    let (a, b) = (mongo_db().await, mongo_db().await);
    assert!(
        tokio::time::timeout(Duration::from_secs(3), mongo_db())
            .await
            .is_err()
    );
    drop(a);
    let c = mongo_db().await;
    assert_eq!(c.count("many").await, 10_000);
    drop(b);
}

#[tokio::test]
async fn open_cursors_sees_only_this_database() {
    let db = mongo_db().await;
    let baseline = db.open_cursors().await;
    let other = db
        .client(MgUser::Root)
        .await
        .database(&format!("{}_other", db.database()));
    let documents = [doc! { "_id": 1 }, doc! { "_id": 2 }];
    other
        .run_command(doc! { "insert": "t", "documents": documents.as_slice() })
        .await
        .unwrap();
    let mut cursor = other
        .run_cursor_command(doc! { "find": "t", "batchSize": 1 })
        .await
        .unwrap();
    assert!(cursor.advance().await.unwrap());
    assert_eq!(db.open_cursors().await, baseline);
    drop(cursor);
    other.drop().await.unwrap();
}

#[tokio::test]
async fn idle_cursors_can_be_killed_behind_the_client() {
    let db = mongo_db().await;
    let baseline = db.open_cursors().await;
    // As the reader, because the tests' cursors are the reader's, not root's.
    let app = db.client(MgUser::Reader).await.database(db.database());
    let mut cursor = app
        .run_cursor_command(doc! { "find": "many" })
        .await
        .unwrap();
    assert!(cursor.advance().await.unwrap());
    assert_eq!(db.open_cursors().await, baseline + 1);
    assert_eq!(db.kill_idle_cursors().await, 1);
    assert_eq!(db.open_cursors().await, baseline);
}

#[tokio::test]
#[should_panic(expected = "writeErrors")]
async fn run_fails_on_a_write_error_in_an_ok_reply() {
    let db = mongo_db().await;
    db.run(doc! { "insert": "many", "documents": [{ "_id": 0 }] })
        .await;
}

#[tokio::test]
async fn an_index_build_on_an_existing_collection_finds_disk_space() {
    let db = mongo_db().await;
    db.run(doc! { "create": "made" }).await;
    let index = doc! { "key": { "a": 1 }, "name": "a_1" };
    db.run(doc! { "createIndexes": "made", "indexes": [index] })
        .await;
}
