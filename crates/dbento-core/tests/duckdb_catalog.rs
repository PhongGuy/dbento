#![cfg(feature = "duckdb")]
//! The DuckDB catalog through `Session`: schemas, relations and columns of the current
//! database, and relation definitions as DuckDB stores them.
use std::time::Duration;

use crate::support::{Cell, duckdb_fixture, run};
use dbento_core::{
    DefinitionError, Limits, ObjectRef, RelationKind, RelationObjects, RelationRef, SchemaObjects,
    Session, SessionOptions,
};

const SETUP: &str = "CREATE SCHEMA s2; \
    CREATE TABLE t(id INTEGER PRIMARY KEY, n VARCHAR NOT NULL DEFAULT 'x', l INTEGER[], d DECIMAL(10,2)); \
    CREATE VIEW v AS SELECT id FROM t; \
    CREATE TABLE b(x INTEGER, y INTEGER, PRIMARY KEY (y, x)); \
    CREATE TABLE s2.\"Mixed Case\"(x INTEGER);";

async fn relation_names(s: &mut Session, schema: &str) -> Vec<(String, RelationKind)> {
    let list = s.relations(schema).await.unwrap();
    assert!(!list.truncated(), "the listing is complete");
    list.items()
        .iter()
        .map(|r| (r.name().to_owned(), r.kind()))
        .collect()
}

/// The app's catalog session: trusted, forced read-only, bounded by a timeout.
fn catalog_options() -> SessionOptions {
    let mut o = SessionOptions::default();
    o.force_read_only = true;
    o.trusted_sql = true;
    o.statement_timeout = Some(Duration::from_secs(5));
    o
}

#[tokio::test]
async fn schemas_are_the_current_databases_own() {
    let fx = duckdb_fixture(SETUP, "");
    let mut s = fx.session(SessionOptions::default()).await;
    run(
        &mut s,
        "CREATE TEMP TABLE tmp(z INTEGER)",
        Limits::default(),
    )
    .await
    .remove(0)
    .unwrap();
    let names: Vec<_> = s
        .schemas()
        .await
        .unwrap()
        .iter()
        .map(|x| x.name().to_owned())
        .collect();
    assert_eq!(names, ["main", "s2"]);
    assert_eq!(
        relation_names(&mut s, "main").await,
        [
            ("b".to_owned(), RelationKind::Table),
            ("t".to_owned(), RelationKind::Table),
            ("v".to_owned(), RelationKind::View)
        ]
    );
    s.close().await.unwrap();
}

#[tokio::test]
async fn relations_match_the_schema_case_insensitively_and_an_unknown_one_is_empty() {
    let fx = duckdb_fixture(SETUP, "");
    let mut s = fx.session(SessionOptions::default()).await;
    assert_eq!(
        relation_names(&mut s, "S2").await,
        [("Mixed Case".to_owned(), RelationKind::Table)]
    );
    assert!(relation_names(&mut s, "nope").await.is_empty());
    s.close().await.unwrap();
}

#[tokio::test]
async fn schemas_and_relations_read_on_a_locked_and_on_a_trusted_timeout_session() {
    let fx = duckdb_fixture(SETUP, "");
    let mut locked = SessionOptions::default();
    locked.force_read_only = true;
    for opts in [locked, catalog_options()] {
        let mut s = fx.session(opts).await;
        assert_eq!(s.schemas().await.unwrap().len(), 2);
        assert_eq!(relation_names(&mut s, "main").await.len(), 3);
        s.close().await.unwrap();
    }
}

#[tokio::test]
async fn a_catalog_read_between_statements_leaves_the_session_usable() {
    let fx = duckdb_fixture(SETUP, "");
    let mut s = fx.session(catalog_options()).await;
    {
        // A parked stream is dropped first, so the catalog read finds the connection free.
        let mut script = s.script("SELECT * FROM range(100000)");
        let pending = script.next().await.unwrap().unwrap();
        let mut park = Limits::default();
        park.max_rows = Some(5);
        park.on_truncate = dbento_core::OnTruncate::Park;
        let mut stream = pending.execute(park).await.unwrap();
        while stream.next_batch().await.unwrap().is_some() {}
    }
    assert_eq!(s.schemas().await.unwrap().len(), 2);
    let r = run(&mut s, "SELECT count(*) FROM t", Limits::default())
        .await
        .remove(0)
        .unwrap();
    assert_eq!(r.rows, vec![vec![Cell::Text("0".to_owned())]]);
    s.close().await.unwrap();
}

type Col = (String, String, bool, Option<u16>, Option<String>);

async fn columns(s: &mut Session, schema: &str, name: &str) -> Vec<Col> {
    s.columns(&RelationRef::new(schema, name))
        .await
        .unwrap()
        .iter()
        .map(|c| {
            (
                c.name().to_owned(),
                c.type_name().to_owned(),
                c.nullable(),
                c.primary_key(),
                c.default().map(str::to_owned),
            )
        })
        .collect()
}

/// DuckDB's own `sql` text for a relation, read through the session.
async fn stored_sql(s: &mut Session, sql: &str) -> String {
    match run(s, sql, Limits::default())
        .await
        .remove(0)
        .unwrap()
        .rows
        .remove(0)
        .remove(0)
    {
        Cell::Text(t) => t,
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn columns_carry_type_nullability_key_position_and_default() {
    let fx = duckdb_fixture(SETUP, "");
    let mut s = fx.session(SessionOptions::default()).await;
    let col = |n: &str, t: &str, null: bool, pk: Option<u16>, d: Option<&str>| -> Col {
        (n.into(), t.into(), null, pk, d.map(str::to_owned))
    };
    assert_eq!(
        columns(&mut s, "main", "t").await,
        [
            col("id", "INTEGER", false, Some(1), None),
            col("n", "VARCHAR", false, None, Some("'x'")),
            col("l", "INTEGER[]", true, None, None),
            col("d", "DECIMAL(10,2)", true, None, None),
        ]
    );
    assert_eq!(
        columns(&mut s, "main", "b").await,
        [
            col("x", "INTEGER", false, Some(2), None),
            col("y", "INTEGER", false, Some(1), None),
        ]
    );
    assert_eq!(columns(&mut s, "MAIN", "T").await.len(), 4);
    assert!(columns(&mut s, "main", "nope").await.is_empty());
    s.close().await.unwrap();
}

#[tokio::test]
async fn a_definition_is_the_sql_duckdb_stores() {
    let fx = duckdb_fixture(SETUP, "");
    let mut s = fx.session(SessionOptions::default()).await;
    for (rel, oracle) in [
        (
            RelationRef::new("main", "t"),
            "SELECT sql FROM duckdb_tables() WHERE table_name = 't'",
        ),
        (
            RelationRef::new("main", "v"),
            "SELECT sql FROM duckdb_views() WHERE view_name = 'v'",
        ),
        (
            RelationRef::new("s2", "mixed case"),
            "SELECT sql FROM duckdb_tables() WHERE table_name = 'Mixed Case'",
        ),
    ] {
        let want = stored_sql(&mut s, oracle).await;
        let def = s
            .definition(&ObjectRef::relation(rel.clone()))
            .await
            .unwrap();
        assert_eq!(def.text(), want, "{rel:?}");
        assert!(!def.truncated() && !def.composed(), "{rel:?}");
    }
    let missing = s
        .definition(&ObjectRef::relation(RelationRef::new("main", "nope")))
        .await;
    assert!(
        matches!(missing, Err(DefinitionError::Missing { .. })),
        "{missing:?}"
    );
    s.close().await.unwrap();
}

#[tokio::test]
async fn columns_and_definitions_read_on_a_locked_and_on_a_trusted_timeout_session() {
    let fx = duckdb_fixture(SETUP, "");
    let mut locked = SessionOptions::default();
    locked.force_read_only = true;
    for opts in [locked, catalog_options()] {
        let mut s = fx.session(opts).await;
        assert_eq!(columns(&mut s, "main", "t").await.len(), 4);
        s.definition(&ObjectRef::relation(RelationRef::new("main", "t")))
            .await
            .unwrap();
        s.close().await.unwrap();
    }
}

#[tokio::test]
async fn objects_are_empty_lists_not_errors() {
    let fx = duckdb_fixture(SETUP, "");
    let mut s = fx.session(catalog_options()).await;
    let none = SchemaObjects::new(Vec::new(), Vec::new(), false);
    assert_eq!(s.schema_objects("main").await.unwrap(), none);
    assert_eq!(s.schema_objects("nope").await.unwrap(), none);
    let empty = RelationObjects::new(Vec::new(), Vec::new(), Vec::new());
    for rel in [
        RelationRef::new("main", "t"),
        RelationRef::new("main", "nope"),
    ] {
        let o = s.relation_objects(&rel).await.unwrap();
        assert_eq!(o, empty, "{rel:?}");
        assert_eq!(o.unreadable_reason(), None, "{rel:?}");
    }
}
