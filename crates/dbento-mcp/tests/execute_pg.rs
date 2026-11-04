//! `execute` on Postgres: classification in a read-only session, then approval.
mod common;
use std::time::Duration;

use common::TestServer;
use dbento_core::{Decision, History, HistoryFilter, HistoryOutcome};
use dbento_testkit::{PgDb, pg_db};
use serde_json::json;

#[tokio::test]
async fn postgres_refuses_transaction_control_and_reads_and_files_writes() {
    let db = pg_db().await;
    db.admin()
        .await
        .batch_execute("CREATE TABLE t(a int)")
        .await
        .unwrap();
    let server = TestServer::postgres(&db, "").await;
    for (sql, code) in [
        ("BEGIN", "transaction_control"),
        ("SET TRANSACTION READ WRITE", "transaction_control"),
        ("SELECT 1", "not_a_write"),
        ("INSERT INTO nope VALUES (1)", "database"),
        ("CREATE TABLE u(a int)", "approval_required"),
        (
            "WITH d AS (DELETE FROM t RETURNING a) SELECT * FROM d",
            "approval_required",
        ),
    ] {
        let e = server
            .err("execute", json!({"connection": "pg", "sql": sql}))
            .await;
        assert_eq!(e["code"], code, "{sql}: {e}");
    }
    assert_eq!(server.approvals().pending().len(), 2);
    assert_eq!(
        db.scalar("SELECT count(*) FROM pg_tables WHERE tablename = 'u'")
            .await
            .as_deref(),
        Some("0")
    );
}

/// Polls `sql` on `db` until it answers `want`, for up to 5 s.
async fn until(db: &PgDb, sql: &str, want: &str) -> bool {
    for _ in 0..50 {
        if db.scalar(sql).await.as_deref() == Some(want) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

#[tokio::test]
async fn postgres_runs_an_approved_update_outside_any_read_only_transaction() {
    let db = pg_db().await;
    db.admin()
        .await
        .batch_execute("CREATE TABLE t(a int); INSERT INTO t VALUES (1), (2)")
        .await
        .unwrap();
    let server = TestServer::postgres(&db, "").await;
    let sql = "UPDATE t SET a = a + 10 WHERE a = 1";
    server.decided("pg", sql, Decision::Approve).await;
    let out = server
        .ok("execute", json!({"connection": "pg", "sql": sql}))
        .await;
    assert_eq!(out["rows_affected"], 1, "{out}");
    assert_eq!(
        db.scalar("SELECT count(*) FROM t WHERE a = 11")
            .await
            .as_deref(),
        Some("1")
    );
}

#[tokio::test]
async fn a_client_cancel_stops_an_approved_statement_in_the_database() {
    let db = pg_db().await;
    db.admin()
        .await
        .batch_execute("CREATE TABLE t(a int); INSERT INTO t VALUES (1)")
        .await
        .unwrap();
    let server = TestServer::postgres(&db, "").await;
    let sql = "UPDATE t SET a = (SELECT 2 FROM pg_sleep(60))";
    server.decided("pg", sql, Decision::Approve).await;
    let active = format!(
        "SELECT count(*) FROM pg_stat_activity WHERE state = 'active' AND query = '{}' AND datname = '{}'",
        sql.replace('\'', "''"),
        db.database()
    );
    let handle = server
        .start_call("execute", json!({"connection": "pg", "sql": sql}))
        .await;
    assert!(until(&db, &active, "1").await, "the update never started");
    handle.cancel(None).await.expect("cancel sent");
    assert!(
        until(&db, &active, "0").await,
        "the update still runs after the cancel"
    );
    assert_eq!(db.scalar("SELECT a FROM t").await.as_deref(), Some("1"));
    let mut recorded = false;
    for _ in 0..50 {
        let entries = History::open(&server.history_file())
            .unwrap()
            .recent(&HistoryFilter::default(), 10);
        if entries
            .iter()
            .any(|e| e.sql() == sql && *e.outcome() == HistoryOutcome::Cancelled)
        {
            recorded = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(recorded, "no Cancelled history entry");
}
