//! The catalog on a real server: databases, collections, views, inferred columns and indexes.
#![cfg(feature = "mongodb")]
mod support;

use bson::doc;
use dbento_core::{ExecuteError, IntrospectError, RelationKind, RelationRef, SessionOptions};
use dbento_testkit::MgUser::{Reader, Root};
use support::{MgFixture, mg};

async fn seeded() -> MgFixture {
    let fx = mg("").await;
    let db = &fx.db;
    db.run(doc! { "create": "tv", "viewOn": "many", "pipeline": [] })
        .await;
    db.run(doc! { "create": "ts", "timeseries": { "timeField": "at" } })
        .await;
    db.run(doc! { "createIndexes": "many", "indexes": [
        { "key": { "n": 1 }, "name": "n_1", "unique": true },
        { "key": { "s": "text" }, "name": "s_text" },
        { "key": { "k": 1 }, "name": "k_1", "partialFilterExpression": { "k": { "$gt": 1 } } },
    ] })
    .await;
    fx
}

#[tokio::test]
async fn a_reader_lists_its_database_and_no_hidden_ones() {
    let fx = seeded().await;
    let mut s = fx.session(Reader, SessionOptions::default()).await;
    let names: Vec<_> = s
        .schemas()
        .await
        .unwrap()
        .iter()
        .map(|i| i.name().to_owned())
        .collect();
    assert!(names.contains(&fx.db.database().to_owned()), "{names:?}");
    assert!(
        !names
            .iter()
            .any(|n| n == "admin" || n == "config" || n == "local"),
        "{names:?}"
    );
}

#[tokio::test]
async fn relations_show_collections_views_and_timeseries_without_system_ones() {
    let fx = seeded().await;
    let mut s = fx.session(Root, SessionOptions::default()).await;
    let list = s.relations(fx.db.database()).await.unwrap();
    let got: Vec<_> = list.items().iter().map(|r| (r.name(), r.kind())).collect();
    assert_eq!(
        got,
        [
            ("many", RelationKind::Table),
            ("torture", RelationKind::Table),
            ("ts", RelationKind::Table),
            ("tv", RelationKind::View),
        ]
    );
    assert!(s.relations("no_such_db").await.unwrap().items().is_empty());
}

#[tokio::test]
async fn columns_come_from_a_sample_and_work_on_a_view() {
    let fx = seeded().await;
    let mut s = fx.session(Reader, SessionOptions::default()).await;
    let many = RelationRef::new(fx.db.database(), "many");
    let columns = s.columns(&many).await.unwrap();
    let got: Vec<_> = columns
        .iter()
        .map(|c| (c.name(), c.type_name(), c.nullable()))
        .collect();
    assert_eq!(got[0], ("_id", "int", false));
    assert!(got.contains(&("s", "string", false)), "{got:?}");
    assert_eq!(columns[0].primary_key(), Some(1));
    let view = RelationRef::new(fx.db.database(), "tv");
    assert_eq!(s.columns(&view).await.unwrap().len(), 3);
    let missing = RelationRef::new(fx.db.database(), "nope");
    assert!(s.columns(&missing).await.unwrap().is_empty());
}

#[tokio::test]
async fn a_collection_the_user_cannot_read_gives_the_reason() {
    let fx = seeded().await;
    let mut s = fx.session(Reader, SessionOptions::default()).await;
    match s.columns(&RelationRef::new("elsewhere", "t")).await {
        Err(IntrospectError::Execute(ExecuteError::Database { code, .. })) => {
            assert_eq!(code, "Unauthorized")
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn relation_objects_list_indexes_and_a_view_or_missing_one_has_none() {
    let fx = seeded().await;
    let mut s = fx.session(Reader, SessionOptions::default()).await;
    let db = fx.db.database();
    let objects = s
        .relation_objects(&RelationRef::new(db, "many"))
        .await
        .unwrap();
    let got: Vec<_> = objects
        .indexes()
        .iter()
        .map(|i| (i.name(), i.unique(), i.partial(), i.method()))
        .collect();
    assert_eq!(
        got,
        [
            ("_id_", true, false, None),
            ("n_1", true, false, None),
            ("s_text", false, false, Some("text")),
            ("k_1", false, true, None),
        ]
    );
    for name in ["tv", "nope"] {
        let none = s
            .relation_objects(&RelationRef::new(db, name))
            .await
            .unwrap();
        assert!(none.indexes().is_empty(), "{name}");
    }
}
