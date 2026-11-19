//! `execute` binds an approval to what the connection points at, not only to its name.
use crate::common::TestServer;
use dbento_core::Decision;
use serde_json::{Value, json};

const SETUP: &str = "CREATE TABLE t(a INTEGER); INSERT INTO t VALUES (1), (2);";
const DELETE: &str = "DELETE FROM t WHERE a = 1";
const APP: &str = "path = \"data/app.db\"";

fn call() -> Value {
    json!({"connection": "lite", "sql": DELETE})
}

/// data/other.db beside data/app.db, holding the same two rows.
fn other_db(server: &TestServer) {
    rusqlite::Connection::open(server.root().join("data/other.db"))
        .unwrap()
        .execute_batch(SETUP)
        .unwrap();
}

fn other_count(server: &TestServer) -> i64 {
    rusqlite::Connection::open(server.root().join("data/other.db"))
        .unwrap()
        .query_row("SELECT count(*) FROM t", [], |r| r.get(0))
        .unwrap()
}

fn edit_toml(server: &TestServer, from: &str, to: &str) {
    let toml = server.root().join("dbento.toml");
    let text = std::fs::read_to_string(&toml).unwrap();
    assert!(text.contains(from), "{text}");
    std::fs::write(&toml, text.replace(from, to)).unwrap();
}

#[tokio::test]
async fn approval_required_names_the_target() {
    let server = TestServer::sqlite(SETUP, "").await;
    let e = server.err("execute", call()).await;
    assert_eq!(e["code"], "approval_required", "{e}");
    let message = e["message"].as_str().unwrap();
    assert!(
        message.contains("data/app.db") && message.contains("Dbento app"),
        "{e}"
    );
}

#[tokio::test]
async fn a_grant_never_runs_against_a_target_changed_after_approval() {
    let server = TestServer::sqlite(SETUP, "").await;
    other_db(&server);
    let old = server.decided("lite", DELETE, Decision::Approve).await;
    edit_toml(&server, APP, "path = \"data/other.db\"");
    let e = server.err("execute", call()).await;
    assert_eq!(e["code"], "connection_changed", "{e}");
    let new = e["request_id"].as_str().unwrap().to_owned();
    assert_ne!(new, old);
    let message = e["message"].as_str().unwrap();
    for part in [
        "\"lite\"",
        old.as_str(),
        new.as_str(),
        "data/other.db",
        "target or session settings",
    ] {
        assert!(message.contains(part), "{part} missing: {message}");
    }
    assert_eq!(server.count("SELECT count(*) FROM t"), 2);
    assert_eq!(other_count(&server), 2);
    let pending = server.approvals().pending();
    assert_eq!(pending.len(), 1);
    assert_eq!(
        (pending[0].id(), pending[0].target().display()),
        (new.as_str(), "data/other.db")
    );
    let kept = server
        .approvals_dir()
        .join("grants")
        .join(format!("{old}.json"));
    assert!(kept.exists(), "the old grant waits out its TTL");
}

#[tokio::test]
async fn a_colour_change_keeps_the_grant() {
    let server = TestServer::sqlite(SETUP, "").await;
    server.decided("lite", DELETE, Decision::Approve).await;
    edit_toml(&server, APP, &format!("{APP}\ncolor = \"red\""));
    let out = server.ok("execute", call()).await;
    assert_eq!(out["rows_affected"], 1, "{out}");
    assert_eq!(server.count("SELECT count(*) FROM t"), 1);
}

#[tokio::test]
async fn a_session_setting_the_display_omits_is_still_a_change() {
    let server = TestServer::sqlite(SETUP, "").await;
    server.decided("lite", DELETE, Decision::Approve).await;
    edit_toml(&server, APP, &format!("{APP}\nstatement_timeout = \"5s\""));
    let e = server.err("execute", call()).await;
    assert_eq!(e["code"], "connection_changed", "{e}");
    assert!(
        e["message"].as_str().unwrap().contains("data/app.db"),
        "{e}"
    );
    assert_eq!(server.count("SELECT count(*) FROM t"), 2);
}

#[tokio::test]
async fn restoring_the_old_target_lets_the_old_grant_run_once() {
    let server = TestServer::sqlite(SETUP, "").await;
    other_db(&server);
    server.decided("lite", DELETE, Decision::Approve).await;
    edit_toml(&server, APP, "path = \"data/other.db\"");
    let e = server.err("execute", call()).await;
    assert_eq!(e["code"], "connection_changed", "{e}");
    edit_toml(&server, "path = \"data/other.db\"", APP);
    let out = server.ok("execute", call()).await;
    assert_eq!(out["rows_affected"], 1, "{out}");
    assert_eq!(server.count("SELECT count(*) FROM t"), 1);
    assert_eq!(other_count(&server), 2);
    let e = server.err("execute", call()).await;
    assert_eq!(e["code"], "approval_required", "the grant was spent: {e}");
}
