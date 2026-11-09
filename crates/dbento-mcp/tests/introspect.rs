//! list_schemas, list_tables and describe_table on SQLite and Postgres, and connect failures.
use crate::common::TestServer;
use dbento_testkit::pg_db;
use serde_json::json;

const SETUP: &str = "CREATE TABLE t(id INTEGER NOT NULL PRIMARY KEY, note TEXT NOT NULL DEFAULT 'x'); CREATE VIEW v AS SELECT id FROM t;";

#[tokio::test]
async fn sqlite_schemas_tables_and_columns() {
    let server = TestServer::sqlite(SETUP, "").await;
    assert_eq!(
        server
            .ok("list_schemas", json!({"connection": "lite"}))
            .await,
        json!({"schemas": [{"name": "main"}]})
    );
    assert_eq!(
        server
            .ok(
                "list_tables",
                json!({"connection": "lite", "schema": "main"})
            )
            .await,
        json!({"tables": [{"name": "t", "kind": "table"}, {"name": "v", "kind": "view"}], "truncated": false})
    );
    assert_eq!(
        server
            .ok(
                "describe_table",
                json!({"connection": "lite", "schema": "main", "table": "t"})
            )
            .await,
        json!({"columns": [
            {"name": "id", "type": "INTEGER", "nullable": false, "primary_key": 1, "default": null, "inferred": false},
            {"name": "note", "type": "TEXT", "nullable": false, "primary_key": null, "default": "'x'", "inferred": false}
        ]})
    );
}

#[tokio::test]
async fn an_unknown_schema_has_no_tables_and_an_unknown_table_no_columns() {
    let server = TestServer::sqlite(SETUP, "").await;
    assert_eq!(
        server
            .ok(
                "list_tables",
                json!({"connection": "lite", "schema": "nope"})
            )
            .await,
        json!({"tables": [], "truncated": false})
    );
    assert_eq!(
        server
            .ok(
                "describe_table",
                json!({"connection": "lite", "schema": "main", "table": "nope"})
            )
            .await,
        json!({"columns": []})
    );
}

#[tokio::test]
async fn unknown_unexposed_and_missing_connections_are_no_connection() {
    let extra = "[connections.gone]\nengine = \"sqlite\"\npath = \"data/gone.db\"\n[connections.hidden]\nengine = \"sqlite\"\npath = \"data/app.db\"";
    let server = TestServer::sqlite_with(SETUP, extra, |o| o.expose(["lite", "gone"])).await;
    let nope = server
        .err("list_schemas", json!({"connection": "nope"}))
        .await;
    let hidden = server
        .err("list_schemas", json!({"connection": "hidden"}))
        .await;
    let gone = server
        .err("list_schemas", json!({"connection": "gone"}))
        .await;
    for e in [&nope, &hidden, &gone] {
        assert_eq!(e["code"], "no_connection", "{e}");
    }
    // An unexposed name reads exactly like one that does not exist.
    assert_eq!(
        hidden["message"].as_str().unwrap().replace("hidden", "X"),
        nope["message"].as_str().unwrap().replace("nope", "X")
    );
    // A missing file is named by its connection, never by a local path.
    let root = server.root().to_string_lossy().into_owned();
    assert!(!gone["message"].as_str().unwrap().contains(&root), "{gone}");
}

#[tokio::test]
async fn postgres_schemas_tables_and_columns() {
    let db = pg_db().await;
    db.admin()
        .await
        .batch_execute("CREATE TABLE t(id int8 PRIMARY KEY, note text DEFAULT 'x'); CREATE VIEW v AS SELECT id FROM t;")
        .await
        .unwrap();
    let server = TestServer::postgres(&db, "").await;
    let schemas = server.ok("list_schemas", json!({"connection": "pg"})).await;
    assert!(
        schemas["schemas"]
            .as_array()
            .unwrap()
            .contains(&json!({"name": "public"})),
        "{schemas}"
    );
    assert_eq!(
        server
            .ok(
                "list_tables",
                json!({"connection": "pg", "schema": "public"})
            )
            .await,
        json!({"tables": [{"name": "t", "kind": "table"}, {"name": "v", "kind": "view"}], "truncated": false})
    );
    assert_eq!(
        server
            .ok(
                "describe_table",
                json!({"connection": "pg", "schema": "public", "table": "t"})
            )
            .await,
        json!({"columns": [
            {"name": "id", "type": "bigint", "nullable": false, "primary_key": 1, "default": null, "inferred": false},
            {"name": "note", "type": "text", "nullable": true, "primary_key": null, "default": "'x'::text", "inferred": false}
        ]})
    );
}

#[tokio::test]
async fn a_missing_secret_is_secret_and_a_wrong_one_is_connect_without_echo() {
    let db = pg_db().await;
    let server = TestServer::postgres_secret(&db, "", None, |o| o).await;
    assert_eq!(
        server
            .err("list_schemas", json!({"connection": "pg"}))
            .await["code"],
        "secret"
    );
    let wrong = "wrong-Secret-4711";
    let server = TestServer::postgres_secret(&db, "", Some(wrong.to_owned()), |o| o).await;
    let e = server
        .err("list_schemas", json!({"connection": "pg"}))
        .await;
    assert_eq!(e["code"], "connect", "{e}");
    assert!(!e.to_string().contains(wrong), "{e}");
}
