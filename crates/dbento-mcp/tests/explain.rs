//! The explain tool: a plan, never an execution, in the same read-only session as `query`.
use crate::common::TestServer;
use dbento_testkit::pg_db;
use serde_json::{Value, json};

const T: &str =
    "CREATE TABLE t(id INTEGER PRIMARY KEY, a INTEGER); INSERT INTO t VALUES (1, 10), (2, 20);";

fn lite(sql: &str) -> Value {
    json!({"connection": "lite", "sql": sql})
}

fn pg(sql: &str) -> Value {
    json!({"connection": "pg", "sql": sql})
}

/// Every cell of the result's rows joined, so a test can look for a plan step.
fn plan_text(r: &Value) -> String {
    r["rows"]
        .as_array()
        .expect("rows")
        .iter()
        .flat_map(|row| row.as_array().expect("row").iter())
        .filter_map(Value::as_str)
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn sqlite_explains_a_select_as_a_query_plan() {
    let server = TestServer::sqlite(T, "").await;
    // A leading line comment, as saved queries often have, is fine.
    let r = server
        .ok("explain", lite("-- by id\nSELECT * FROM t WHERE id = 1"))
        .await;
    assert!(plan_text(&r).contains("SEARCH t"), "{r}");
    let r = server
        .ok("explain", lite("SELECT * FROM t WHERE id = 1"))
        .await;
    assert!(plan_text(&r).contains("SEARCH t"), "{r}");
    assert_eq!(r["sql"], "EXPLAIN QUERY PLAN SELECT * FROM t WHERE id = 1");
}

#[tokio::test]
async fn sqlite_previews_a_mutation_without_running_it() {
    let server = TestServer::sqlite(T, "").await;
    for sql in [
        "DELETE FROM t",
        "UPDATE t SET a = 0",
        "INSERT INTO t VALUES (3, 30)",
        "INSERT INTO t SELECT id + 10, a FROM t",
    ] {
        server.ok("explain", lite(sql)).await;
    }
    let r = server
        .ok("explain", lite("DELETE FROM t WHERE a > 0"))
        .await;
    assert!(plan_text(&r).contains("SCAN t"), "{r}");
    assert_eq!(server.count("SELECT count(*) FROM t"), 2);
    assert_eq!(server.count("SELECT sum(a) FROM t"), 30);
}

#[tokio::test]
async fn analyze_a_second_statement_or_a_nested_explain_is_refused() {
    let server = TestServer::sqlite(T, "").await;
    for (sql, code) in [
        ("SELECT 1; DELETE FROM t", "one_statement"),
        ("", "one_statement"),
        ("ANALYZE", "not_explainable"),
        ("ANALYZE t", "not_explainable"),
        ("EXPLAIN DELETE FROM t", "not_explainable"),
        ("explain query plan DELETE FROM t", "not_explainable"),
        ("-- hi\nANALYZE DELETE FROM t", "not_explainable"),
        ("(ANALYZE) DELETE FROM t", "not_explainable"),
        ("/* x */ ANALYZE t", "not_explainable"),
        ("DROP TABLE t", "not_explainable"),
        ("CREATE TABLE u(a)", "not_explainable"),
        ("PRAGMA query_only = 0", "not_explainable"),
        ("BEGIN", "not_explainable"),
    ] {
        let e = server.err("explain", lite(sql)).await;
        assert_eq!(e["code"], code, "{sql:?}: {e}");
    }
    assert_eq!(server.count("SELECT count(*) FROM t"), 2);
    assert_eq!(
        server.count("SELECT count(*) FROM sqlite_master WHERE name = 't'"),
        1
    );
}

#[tokio::test]
async fn the_explain_tool_is_listed_read_only_and_says_it_never_runs_the_statement() {
    let server = TestServer::sqlite("", "").await;
    let tool = server
        .tools()
        .await
        .into_iter()
        .find(|t| t.name == "explain")
        .expect("explain is listed");
    let hints = tool.annotations.expect("annotations");
    assert_eq!(hints.read_only_hint, Some(true));
    let description = tool.description.expect("description");
    assert!(description.contains("ANALYZE"), "{description}");
}

#[tokio::test]
async fn postgres_explains_a_select_and_previews_mutations_without_running_them() {
    let db = pg_db().await;
    db.admin()
        .await
        .batch_execute(
            "CREATE TABLE t(id int PRIMARY KEY, a int); INSERT INTO t VALUES (1, 10), (2, 20)",
        )
        .await
        .unwrap();
    let server = TestServer::postgres(&db, "").await;
    let r = server.ok("explain", pg("SELECT * FROM t")).await;
    assert_eq!(r["columns"][0]["name"], "QUERY PLAN", "{r}");
    assert!(plan_text(&r).contains("Seq Scan on t"), "{r}");
    assert_eq!(r["sql"], "EXPLAIN SELECT * FROM t");
    for sql in [
        "DELETE FROM t",
        "UPDATE t SET a = 0",
        "INSERT INTO t VALUES (3, 30)",
        "WITH d AS (DELETE FROM t RETURNING id) SELECT * FROM d",
    ] {
        let r = server.ok("explain", pg(sql)).await;
        assert!(!plan_text(&r).is_empty(), "{sql}: {r}");
    }
    assert_eq!(
        db.scalar("SELECT count(*) FROM t").await.as_deref(),
        Some("2")
    );
    assert_eq!(
        db.scalar("SELECT sum(a) FROM t").await.as_deref(),
        Some("30")
    );
}

#[tokio::test]
async fn postgres_refuses_analyze_options_and_smuggled_statements() {
    let db = pg_db().await;
    db.admin()
        .await
        .batch_execute("CREATE TABLE t(id int); INSERT INTO t VALUES (1), (2)")
        .await
        .unwrap();
    let server = TestServer::postgres(&db, "").await;
    for (sql, code) in [
        ("ANALYZE DELETE FROM t", "not_explainable"),
        ("ANALYSE DELETE FROM t", "not_explainable"),
        ("(ANALYZE) DELETE FROM t", "not_explainable"),
        (
            "(ANALYZE true, FORMAT json) DELETE FROM t",
            "not_explainable",
        ),
        // Postgres nests block comments, so a scanner that stops at the first */ is fooled.
        (
            "/* /* */ SELECT */ ANALYZE DELETE FROM t",
            "not_explainable",
        ),
        (
            "/* /* */ SELECT */ (ANALYZE) DELETE FROM t",
            "not_explainable",
        ),
        ("/* note */ SELECT 1", "not_explainable"),
        ("SELECT_x FROM t", "not_explainable"),
        ("VERBOSE DELETE FROM t", "not_explainable"),
        ("EXPLAIN ANALYZE DELETE FROM t", "not_explainable"),
        ("SELECT 1; DELETE FROM t", "one_statement"),
        ("SELECT 1; COMMIT; DELETE FROM t", "one_statement"),
        ("DROP TABLE t", "not_explainable"),
        ("TRUNCATE t", "not_explainable"),
        ("COMMIT", "not_explainable"),
    ] {
        let e = server.err("explain", pg(sql)).await;
        assert_eq!(e["code"], code, "{sql:?}: {e}");
    }
    assert_eq!(
        db.scalar("SELECT count(*) FROM t").await.as_deref(),
        Some("2")
    );
}

#[tokio::test]
async fn postgres_explain_cannot_write_through_a_volatile_function() {
    // Even a statement the allowlist accepts runs read-only: a SELECT calling a writing
    // function is planned, not executed, so the function never runs.
    let db = pg_db().await;
    db.admin()
        .await
        .batch_execute(
            "CREATE TABLE t(id int); INSERT INTO t VALUES (1);
             CREATE FUNCTION wipe() RETURNS int LANGUAGE sql AS 'DELETE FROM t RETURNING 1';",
        )
        .await
        .unwrap();
    let server = TestServer::postgres(&db, "").await;
    server.ok("explain", pg("SELECT wipe()")).await;
    assert_eq!(
        db.scalar("SELECT count(*) FROM t").await.as_deref(),
        Some("1")
    );
}
