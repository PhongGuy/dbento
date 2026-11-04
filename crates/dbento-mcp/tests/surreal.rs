//! SurrealDB through the MCP tools: the one-statement gate, the read-only level and
//! rolled_back on query results, and inferred columns.
mod common;
use common::TestServer;
use dbento_testkit::surreal_db;
use serde_json::json;

// No server at this host: the gate answers before any connect.
const NOWHERE: &str = "dbento = 1\n[connections.sr]\nengine = \"surrealdb\"\nhost = \"db.invalid\"\n\
                       namespace = \"n\"\ndatabase = \"d\"\n";

#[tokio::test]
async fn a_let_or_begin_unit_gets_the_one_statement_error() {
    let server = TestServer::start(NOWHERE).await;
    for sql in [
        "LET $a = 1; RETURN $a",
        "BEGIN; CREATE t:1; COMMIT;",
        "RETURN 1; RETURN 2",
    ] {
        let e = server
            .err("query", json!({"connection": "sr", "sql": sql}))
            .await;
        assert_eq!(e["code"], "one_statement", "{sql:?}: {e}");
    }
    let e = server
        .err(
            "execute",
            json!({"connection": "sr", "sql": "LET $a = 1; CREATE t:1"}),
        )
        .await;
    assert_eq!(e["code"], "one_statement", "{e}");
    let e = server
        .err("query", json!({"connection": "sr", "sql": "RETURN 1"}))
        .await;
    assert_ne!(
        e["code"], "one_statement",
        "one statement reaches the connect: {e}"
    );
}

#[tokio::test]
async fn a_query_result_carries_its_level_and_rolled_back() {
    let db = surreal_db().await;
    db.admin("DEFINE FUNCTION fn::one() { RETURN 1; }; CREATE person:1 SET name = 'ann';")
        .await;
    let root = TestServer::surreal(&db, ("root", "root"), "").await;
    let q = |sql: &str| json!({"connection": "sr", "sql": sql});
    let r = root.ok("query", q("RETURN fn::one()")).await;
    assert_eq!(
        (&r["rolled_back"], &r["read_only_enforcement"]),
        (&json!(true), &json!("best_effort")),
        "{r}"
    );
    let r = root.ok("query", q("SELECT name FROM person")).await;
    assert_eq!(r["rolled_back"], false, "{r}");
    let viewer = TestServer::surreal(&db, ("dv", "database"), "").await;
    let r = viewer.ok("query", q("SELECT name FROM person")).await;
    assert_eq!(r["read_only_enforcement"], "database", "{r}");
    assert_eq!(r["rows"], json!([["ann"]]), "{r}");
}

#[tokio::test]
async fn describe_table_marks_sampled_columns_inferred() {
    let db = surreal_db().await;
    db.admin("DEFINE TABLE loose SCHEMALESS; CREATE loose:1 SET a = 1;")
        .await;
    let server = TestServer::surreal(&db, ("root", "root"), "").await;
    let r = server
        .ok(
            "describe_table",
            json!({"connection": "sr", "schema": "app", "table": "loose"}),
        )
        .await;
    let cols: Vec<_> = r["columns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| (c["name"].clone(), c["inferred"].clone()))
        .collect();
    assert_eq!(
        cols,
        [(json!("id"), json!(false)), (json!("a"), json!(true))],
        "{r}"
    );
}

#[tokio::test]
async fn a_single_surrealdb_write_passes_the_gate_to_approval() {
    let db = surreal_db().await;
    let server = TestServer::surreal(&db, ("root", "root"), "").await;
    let e = server
        .err(
            "execute",
            json!({"connection": "sr", "sql": "CREATE t:1 SET x = 1"}),
        )
        .await;
    assert_eq!(e["code"], "approval_required", "{e}");
    assert!(e["request_id"].is_string(), "{e}");
    assert_eq!(db.count("t").await, 0, "nothing ran before approval");
}

#[tokio::test]
async fn other_engines_report_their_level_and_no_rollback() {
    let server = TestServer::sqlite("CREATE TABLE t(x)", "").await;
    let r = server
        .ok("query", json!({"connection": "lite", "sql": "SELECT 1"}))
        .await;
    assert_eq!(
        (&r["rolled_back"], &r["read_only_enforcement"]),
        (&json!(false), &json!("database")),
        "{r}"
    );
}
