//! `execute` refusals and approval requests on SQLite; nothing runs before an approval.
mod common;
use common::TestServer;
use dbento_core::{Unguarded, Writes, approval_id};
use serde_json::{Value, json};

const SETUP: &str = "CREATE TABLE t(a INTEGER);";

async fn exec(server: &TestServer, sql: &str) -> Value {
    server
        .err("execute", json!({"connection": "lite", "sql": sql}))
        .await
}

#[tokio::test]
async fn a_write_files_a_request_and_runs_nothing() {
    let server = TestServer::sqlite(SETUP, "").await;
    let e = exec(&server, "INSERT INTO t VALUES (1)").await;
    assert_eq!(e["code"], "approval_required", "{e}");
    let root = std::fs::canonicalize(server.root()).unwrap();
    let id = approval_id(&root, "lite", "INSERT INTO t VALUES (1)");
    assert_eq!(e["request_id"], id.as_str());
    assert!(e["message"].as_str().unwrap().contains("Dbento app"), "{e}");
    let pending = server.approvals().pending();
    assert_eq!(pending.len(), 1);
    let p = &pending[0];
    assert_eq!(
        (p.id(), p.workspace(), p.connection(), p.sql(), p.writes()),
        (
            id.as_str(),
            root.as_path(),
            "lite",
            "INSERT INTO t VALUES (1)",
            Writes::Yes
        )
    );
    assert_eq!(server.count("SELECT count(*) FROM t"), 0);
}

#[tokio::test]
async fn the_identical_retry_keeps_one_request_and_any_change_files_another() {
    let server = TestServer::sqlite(SETUP, "").await;
    let first = exec(&server, "DELETE FROM t").await;
    let again = exec(&server, "DELETE FROM t").await;
    assert_eq!(again["code"], "approval_required");
    assert_eq!(first["request_id"], again["request_id"]);
    assert_eq!(server.approvals().pending().len(), 1);
    let spaced = exec(&server, "DELETE FROM t ").await;
    assert_ne!(first["request_id"], spaced["request_id"]);
    let pending = server.approvals().pending();
    assert_eq!(pending.len(), 2);
    assert!(
        pending
            .iter()
            .all(|r| r.unguarded() == [Unguarded::DeleteWithoutWhere])
    );
}

#[tokio::test]
async fn refusals_file_nothing() {
    let server = TestServer::sqlite(SETUP, "").await;
    for (sql, code) in [
        ("SELECT a FROM t", "not_a_write"),
        ("BEGIN", "transaction_control"),
        ("COMMIT", "transaction_control"),
        ("SAVEPOINT s", "transaction_control"),
        (
            "INSERT INTO t VALUES (1); INSERT INTO t VALUES (2)",
            "one_statement",
        ),
        ("", "one_statement"),
        ("-- only a comment", "one_statement"),
        ("INSERT INTO nope VALUES (1)", "database"),
    ] {
        let e = exec(&server, sql).await;
        assert_eq!(e["code"], code, "{sql}: {e}");
    }
    let e = server
        .err(
            "execute",
            json!({"connection": "nope", "sql": "DELETE FROM t"}),
        )
        .await;
    assert_eq!(e["code"], "no_connection");
    let e = exec(&server, "SELECT 1").await;
    assert!(e["message"].as_str().unwrap().contains("query"), "{e}");
    assert!(server.approvals().pending().is_empty());
}

#[tokio::test]
async fn a_read_only_connection_can_never_be_asked_for() {
    let server = TestServer::sqlite(SETUP, "read_only = true").await;
    let e = exec(&server, "INSERT INTO t VALUES (1)").await;
    assert_eq!(e["code"], "read_only_connection", "{e}");
    assert!(server.approvals().pending().is_empty());
}

#[tokio::test]
async fn an_unusable_approvals_folder_is_approvals_unavailable_and_query_still_works() {
    let blocker = tempfile::NamedTempFile::new().unwrap();
    let under_a_file = blocker.path().join("approvals");
    let server = TestServer::sqlite_with(SETUP, "", move |o| o.approvals_dir(under_a_file)).await;
    let e = exec(&server, "INSERT INTO t VALUES (1)").await;
    assert_eq!(e["code"], "approvals_unavailable", "{e}");
    // The client never learns a local path; the detail goes to stderr.
    let parent = blocker
        .path()
        .parent()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    assert!(!e["message"].as_str().unwrap().contains(&parent), "{e}");
    let q = server
        .ok(
            "query",
            json!({"connection": "lite", "sql": "SELECT count(*) FROM t"}),
        )
        .await;
    assert_eq!(q["rows"], json!([["0"]]));
}
