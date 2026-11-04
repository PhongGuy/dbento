//! Approved, denied and expired calls on SQLite, and what the history records.
mod common;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use common::TestServer;
use dbento_core::{Decision, History, HistoryFilter, HistoryOutcome, HistorySource};
use serde_json::{Value, json};

const SETUP: &str = "CREATE TABLE t(a INTEGER); INSERT INTO t VALUES (1), (2);";

fn call(sql: &str) -> Value {
    json!({"connection": "lite", "sql": sql})
}

#[tokio::test]
async fn an_approved_call_runs_once_in_a_read_write_session() {
    let server = TestServer::sqlite(SETUP, "").await;
    server
        .decided("lite", "DELETE FROM t WHERE a = 1", Decision::Approve)
        .await;
    let out = server
        .ok("execute", call("DELETE FROM t WHERE a = 1"))
        .await;
    assert_eq!(out["rows_affected"], 1, "{out}");
    assert!(out["duration_ms"].is_u64(), "{out}");
    assert_eq!(server.count("SELECT count(*) FROM t"), 1);
    let e = server
        .err("execute", call("DELETE FROM t WHERE a = 1"))
        .await;
    assert_eq!(e["code"], "approval_required", "the grant was used up: {e}");
}

#[tokio::test]
async fn a_grant_for_one_text_does_not_cover_another() {
    let server = TestServer::sqlite(SETUP, "").await;
    server
        .decided("lite", "DELETE FROM t WHERE a = 1", Decision::Approve)
        .await;
    let e = server
        .err("execute", call("DELETE FROM t WHERE a = 2"))
        .await;
    assert_eq!(e["code"], "approval_required");
    assert_eq!(server.count("SELECT count(*) FROM t"), 2);
}

#[tokio::test]
async fn ddl_runs_and_returned_rows_are_not_sent_back() {
    let server = TestServer::sqlite(SETUP, "").await;
    server
        .decided("lite", "CREATE TABLE u(b TEXT)", Decision::Approve)
        .await;
    server.ok("execute", call("CREATE TABLE u(b TEXT)")).await;
    assert_eq!(
        server.count("SELECT count(*) FROM sqlite_master WHERE name = 'u'"),
        1
    );
    let returning = "INSERT INTO t VALUES (3) RETURNING a";
    server.decided("lite", returning, Decision::Approve).await;
    let out = server.ok("execute", call(returning)).await;
    assert_eq!(out["rows_affected"], Value::Null, "{out}");
    assert!(out.get("rows").is_none(), "{out}");
    assert_eq!(server.count("SELECT count(*) FROM t"), 3);
}

#[tokio::test]
async fn a_denial_answers_the_next_identical_call_once() {
    let server = TestServer::sqlite(SETUP, "").await;
    let id = server
        .decided("lite", "DELETE FROM t", Decision::Deny)
        .await;
    let e = server.err("execute", call("DELETE FROM t")).await;
    assert_eq!(e["code"], "denied", "{e}");
    assert_eq!(e["request_id"], id.as_str());
    let e = server.err("execute", call("DELETE FROM t")).await;
    assert_eq!(e["code"], "approval_required");
    assert_eq!(server.count("SELECT count(*) FROM t"), 2);
}

#[tokio::test]
async fn a_grant_older_than_an_hour_is_not_used() {
    let server = TestServer::sqlite(SETUP, "").await;
    let id = server
        .decided("lite", "DELETE FROM t", Decision::Approve)
        .await;
    let old = SystemTime::now() - Duration::from_secs(3601);
    let at_ms = old.duration_since(UNIX_EPOCH).unwrap().as_millis() as u64;
    std::fs::write(
        server
            .approvals_dir()
            .join("grants")
            .join(format!("{id}.json")),
        json!({"v": 1, "id": id, "at_ms": at_ms}).to_string(),
    )
    .unwrap();
    let e = server.err("execute", call("DELETE FROM t")).await;
    assert_eq!(e["code"], "approval_required");
    assert_eq!(server.count("SELECT count(*) FROM t"), 2);
}

#[tokio::test]
async fn a_connection_made_read_only_after_approval_refuses_and_keeps_the_grant() {
    let server = TestServer::sqlite(SETUP, "").await;
    let id = server
        .decided("lite", "DELETE FROM t", Decision::Approve)
        .await;
    let toml = server.root().join("dbento.toml");
    let text = std::fs::read_to_string(&toml).unwrap().replace(
        "path = \"data/app.db\"",
        "path = \"data/app.db\"\nread_only = true",
    );
    std::fs::write(&toml, text).unwrap();
    let e = server.err("execute", call("DELETE FROM t")).await;
    assert_eq!(e["code"], "read_only_connection", "{e}");
    assert!(
        server
            .approvals_dir()
            .join("grants")
            .join(format!("{id}.json"))
            .exists()
    );
    assert_eq!(server.count("SELECT count(*) FROM t"), 2);
}

#[tokio::test]
async fn executed_calls_are_recorded_as_mcp_and_refusals_are_not() {
    let server = TestServer::sqlite(
        "CREATE TABLE t(a INTEGER PRIMARY KEY); INSERT INTO t VALUES (1);",
        "",
    )
    .await;
    server.err("execute", call("SELECT 1")).await;
    server
        .decided("lite", "DELETE FROM t WHERE a = 1", Decision::Approve)
        .await;
    server
        .ok("execute", call("DELETE FROM t WHERE a = 1"))
        .await;
    server
        .decided("lite", "INSERT INTO t VALUES (5), (5)", Decision::Approve)
        .await;
    let e = server
        .err("execute", call("INSERT INTO t VALUES (5), (5)"))
        .await;
    assert_eq!(e["code"], "database", "{e}");
    let entries = History::open(&server.history_file())
        .unwrap()
        .recent(&HistoryFilter::default(), 100);
    assert_eq!(entries.len(), 2, "{entries:?}");
    assert!(entries.iter().all(|e| e.source() == HistorySource::Mcp));
    assert_eq!(entries[1].sql(), "DELETE FROM t WHERE a = 1");
    assert_eq!(
        *entries[1].outcome(),
        HistoryOutcome::Ok {
            rows: 0,
            truncated: false,
            rows_affected: Some(1)
        }
    );
    assert_eq!(entries[0].sql(), "INSERT INTO t VALUES (5), (5)");
    assert!(matches!(
        entries[0].outcome(),
        HistoryOutcome::Failed { .. }
    ));
}

#[tokio::test]
async fn a_denial_returns_the_users_reason_beside_denied() {
    let server = TestServer::sqlite(SETUP, "").await;
    let e = server.err("execute", call("DELETE FROM t")).await;
    let id = e["request_id"].as_str().expect("request_id").to_owned();
    server
        .approvals()
        .decide_with_reason(
            &id,
            Decision::Deny,
            Some("keep the rows; archive them instead"),
        )
        .unwrap();
    let e = server.err("execute", call("DELETE FROM t")).await;
    assert_eq!(e["code"], "denied", "{e}");
    assert_eq!(e["reason"], "keep the rows; archive them instead", "{e}");
    assert!(e["message"].as_str().unwrap().contains("archive"), "{e}");
    assert_eq!(server.count("SELECT count(*) FROM t"), 2);
}

#[tokio::test]
async fn a_denial_without_a_reason_has_no_reason_field() {
    let server = TestServer::sqlite(SETUP, "").await;
    server
        .decided("lite", "DELETE FROM t", Decision::Deny)
        .await;
    let e = server.err("execute", call("DELETE FROM t")).await;
    assert_eq!(e["code"], "denied", "{e}");
    assert!(e.get("reason").is_none(), "{e}");
}
