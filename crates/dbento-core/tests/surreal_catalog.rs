//! The SurrealDB catalog on a real server: databases, tables and views, declared and
//! inferred columns, indexes and events, at root, namespace and database level.
mod support;

use dbento_core::{
    ColumnMeta, Limits, RelationKind, RelationRef, Session, SessionOptions, TriggerState,
};
use dbento_testkit::{SrDb, surreal_db};
use support::{run, sr_root, sr_session, text};

// The view is defined before the row so it holds it; the event after, so no `audit` table appears.
const SETUP: &str = "DEFINE TABLE person SCHEMAFULL; \
    DEFINE FIELD name ON person TYPE string; \
    DEFINE FIELD age ON person TYPE option<int> DEFAULT 18; \
    DEFINE FIELD addr ON person TYPE object FLEXIBLE; \
    DEFINE FIELD addr.city ON person TYPE string; \
    DEFINE INDEX ix_age ON person FIELDS age; \
    DEFINE INDEX ix_name ON person FIELDS name UNIQUE; \
    DEFINE TABLE adults AS SELECT name, age FROM person WHERE age >= 18; \
    CREATE person:1 SET name = 'ann', age = 20, addr = { city: 'x' }; \
    DEFINE EVENT ev ON person WHEN $event = 'CREATE' THEN { CREATE audit SET x = 1 }; \
    DEFINE TABLE loose SCHEMALESS; \
    CREATE loose:1 SET a = 1, b = 'x'; CREATE loose:2 SET c = true; \
    DEFINE DATABASE other; USE DB other; \
    DEFINE TABLE elsewhere SCHEMALESS; CREATE elsewhere:1 SET z = 1;";

async fn fixture() -> SrDb {
    let db = surreal_db().await;
    db.admin(SETUP).await;
    db
}

fn names(columns: &[ColumnMeta]) -> Vec<&str> {
    columns.iter().map(ColumnMeta::name).collect()
}

async fn schema_names(s: &mut Session) -> Vec<String> {
    s.schemas()
        .await
        .unwrap()
        .iter()
        .map(|x| x.name().to_owned())
        .collect()
}

async fn relation_names(s: &mut Session, schema: &str) -> Vec<String> {
    let list = s.relations(schema).await.unwrap();
    list.items().iter().map(|r| r.name().to_owned()).collect()
}

async fn current_db(s: &mut Session) -> support::Cell {
    let ran = run(s, "RETURN session::db()", Limits::default()).await;
    ran[0].as_ref().unwrap().rows[0][0].clone()
}

#[tokio::test]
async fn root_lists_databases_tables_and_views_and_never_creates_a_database() {
    let db = fixture().await;
    let mut s = sr_root(&db, "", SessionOptions::default()).await;
    assert_eq!(schema_names(&mut s).await, ["app", "other"]);
    let app = s.relations("app").await.unwrap();
    let listed: Vec<_> = app.items().iter().map(|r| (r.name(), r.kind())).collect();
    assert_eq!(
        listed,
        [
            ("adults", RelationKind::View),
            ("loose", RelationKind::Table),
            ("person", RelationKind::Table)
        ]
    );
    assert!(!app.truncated());
    assert_eq!(relation_names(&mut s, "other").await, ["elsewhere"]);
    assert!(relation_names(&mut s, "nowhere").await.is_empty());
    let ns = db.admin("INFO FOR NS").await.pop().unwrap();
    assert!(ns["databases"].get("nowhere").is_none(), "{ns}");
    assert_eq!(
        current_db(&mut s).await,
        text("app"),
        "listing another database moves nothing"
    );
    s.close().await.unwrap();
}

#[tokio::test]
async fn declared_columns_read_kind_default_and_nullability() {
    let db = fixture().await;
    let mut s = sr_root(&db, "", SessionOptions::default()).await;
    let cols = s.columns(&RelationRef::new("app", "person")).await.unwrap();
    assert_eq!(names(&cols), ["id", "addr", "addr.city", "age", "name"]);
    assert_eq!(
        (
            cols[0].type_name(),
            cols[0].primary_key(),
            cols[0].nullable()
        ),
        ("record<person>", Some(1), false)
    );
    assert_eq!(
        (cols[3].type_name(), cols[3].nullable(), cols[3].default()),
        ("none | int", true, Some("18"))
    );
    assert!(!cols[4].nullable());
    assert!(cols.iter().all(|c| !c.inferred()));
    assert!(
        s.columns(&RelationRef::new("app", "nosuch"))
            .await
            .unwrap()
            .is_empty()
    );
    assert!(
        s.columns(&RelationRef::new("nowhere", "person"))
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn schemaless_tables_and_views_add_sampled_columns_marked_inferred() {
    let db = fixture().await;
    let mut s = sr_root(&db, "", SessionOptions::default()).await;
    let loose = s.columns(&RelationRef::new("app", "loose")).await.unwrap();
    assert_eq!(names(&loose), ["id", "a", "b", "c"]);
    assert!(!loose[0].inferred());
    assert!(loose[1..].iter().all(|c| c.inferred() && c.nullable()));
    assert_eq!(
        (loose[1].type_name(), loose[3].type_name()),
        ("int", "bool")
    );
    let adults = s.columns(&RelationRef::new("app", "adults")).await.unwrap();
    assert_eq!(names(&adults), ["id", "age", "name"]);
    assert!(adults[1].inferred() && adults[2].inferred());
    let elsewhere = s
        .columns(&RelationRef::new("other", "elsewhere"))
        .await
        .unwrap();
    assert_eq!(names(&elsewhere), ["id", "z"]);
    assert_eq!(db.count("loose").await, 2, "the sample changed nothing");
}

#[tokio::test]
async fn indexes_and_events_are_the_relation_objects() {
    let db = fixture().await;
    let mut s = sr_root(&db, "", SessionOptions::default()).await;
    let objects = s
        .relation_objects(&RelationRef::new("app", "person"))
        .await
        .unwrap();
    let indexes: Vec<_> = objects
        .indexes()
        .iter()
        .map(|i| (i.name(), i.columns().to_vec(), i.unique()))
        .collect();
    assert_eq!(
        indexes,
        [
            ("ix_age", vec!["age".to_owned()], false),
            ("ix_name", vec!["name".to_owned()], true)
        ]
    );
    let triggers: Vec<_> = objects
        .triggers()
        .iter()
        .map(|t| (t.name(), t.state()))
        .collect();
    assert_eq!(triggers, [("ev", TriggerState::Enabled)]);
    let none = s
        .relation_objects(&RelationRef::new("app", "nosuch"))
        .await
        .unwrap();
    assert!(none.indexes().is_empty() && none.triggers().is_empty());
}

#[tokio::test]
async fn a_database_login_sees_only_its_own_database() {
    let db = fixture().await;
    let mut s = sr_session(&db, Some(("dv", "database")), "", SessionOptions::default()).await;
    assert_eq!(schema_names(&mut s).await, ["app"]);
    assert!(relation_names(&mut s, "other").await.is_empty());
    assert_eq!(
        relation_names(&mut s, "app").await,
        ["adults", "loose", "person"]
    );
    assert_eq!(
        s.columns(&RelationRef::new("app", "person"))
            .await
            .unwrap()
            .len(),
        5
    );
    assert_eq!(current_db(&mut s).await, text("app"));
}

#[tokio::test]
async fn a_namespace_viewer_reads_another_database_in_a_read_only_session() {
    let db = fixture().await;
    let mut opts = SessionOptions::default();
    opts.force_read_only = true;
    let mut s = sr_session(&db, Some(("nv", "namespace")), "", opts).await;
    assert_eq!(schema_names(&mut s).await, ["app", "other"]);
    assert_eq!(relation_names(&mut s, "other").await, ["elsewhere"]);
    let cols = s
        .columns(&RelationRef::new("other", "elsewhere"))
        .await
        .unwrap();
    assert_eq!(names(&cols), ["id", "z"]);
    assert_eq!(current_db(&mut s).await, text("app"));
}
