//! The MCP tools on a DuckDB connection: reads on the locked instance with the server's
//! timeout, and an approved write through the classify-then-write sequence.
#![cfg(feature = "duckdb")]
mod common;
use common::TestServer;
use dbento_core::Decision;
use serde_json::{Value, json};

const DUCK_TOML: &str =
    "dbento = 1\n[connections.duck]\nengine = \"duckdb\"\npath = \"data/app.duckdb\"\n";

fn q(sql: &str) -> Value {
    json!({"connection": "duck", "sql": sql})
}

/// A server on a workspace whose `duck` file is built by running `setup` with the duckdb
/// crate (DuckDB rejects an empty file).
async fn duck_server(setup: &str) -> TestServer {
    let server = TestServer::start(DUCK_TOML).await;
    std::fs::create_dir(server.root().join("data")).unwrap();
    dbento_testkit::duckdb_file(&server.root().join("data/app.duckdb"), setup);
    server
}

#[tokio::test]
async fn the_tool_descriptions_name_duckdb() {
    let server = duck_server("SELECT 1").await;
    let tools = server.tools().await;
    let description = |name: &str| {
        tools
            .iter()
            .find(|t| t.name == name)
            .and_then(|t| t.description.as_deref())
            .unwrap_or_default()
            .to_owned()
    };
    assert!(description("list_connections").contains("duckdb"));
    assert!(description("list_schemas").contains("DuckDB"));
    assert!(description("explain").contains("DuckDB"));
    let e = server.err("explain", q("SELECT 1")).await;
    assert_eq!(e["code"], "unsupported", "{e}");
}

#[tokio::test]
async fn an_approved_write_then_reads_on_the_locked_instance() {
    let server = duck_server("CREATE TABLE t(a INTEGER PRIMARY KEY, l INTEGER[])").await;
    // execute classifies on a locked session, which must close before the writable one opens.
    let insert = "INSERT INTO t VALUES (1, [1, 2]), (2, NULL)";
    server.decided("duck", insert, Decision::Approve).await;
    let out = server.ok("execute", q(insert)).await;
    assert_eq!(out["rows_affected"], 2, "{out}");
    let listed = server.ok("list_connections", json!({})).await;
    assert_eq!(listed["connections"][0]["engine"], "duckdb", "{listed}");
    let out = server.ok("query", q("SELECT a, l FROM t ORDER BY a")).await;
    assert_eq!(out["rows"], json!([["1", "[1, 2]"], ["2", null]]), "{out}");
    assert_eq!(
        server
            .ok("list_schemas", json!({"connection": "duck"}))
            .await,
        json!({"schemas": [{"name": "main"}]})
    );
    assert_eq!(
        server
            .ok(
                "list_tables",
                json!({"connection": "duck", "schema": "main"})
            )
            .await,
        json!({"tables": [{"name": "t", "kind": "table"}], "truncated": false})
    );
    assert_eq!(
        server
            .ok(
                "describe_table",
                json!({"connection": "duck", "schema": "main", "table": "t"})
            )
            .await,
        json!({"columns": [
            {"name": "a", "type": "INTEGER", "nullable": false, "primary_key": 1, "default": null, "inferred": false},
            {"name": "l", "type": "INTEGER[]", "nullable": true, "primary_key": null, "default": null, "inferred": false}
        ]})
    );
}

#[tokio::test]
async fn a_query_cannot_write_or_reach_files() {
    let server = duck_server("CREATE TABLE t(a INTEGER)").await;
    // An absolute path under the server root: a relative one resolves against the cwd.
    let out = server.root().join("out.csv");
    for sql in [
        "INSERT INTO t VALUES (1)".to_owned(),
        format!("COPY t TO '{}'", out.display()),
        "SET GLOBAL threads = 1".to_owned(),
    ] {
        let e = server.err("query", q(&sql)).await;
        assert_eq!(e["code"], "read_only", "{sql}: {e}");
    }
    assert!(!out.exists());
}
