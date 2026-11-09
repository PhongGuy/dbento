//! Each known route out of the read-only wrapper, including a ROLLBACK over a COMMIT. Every
//! attempt must leave the rows, the listener, the session's statement_timeout and the advisory
//! locks as they were.
use std::time::Duration;

use crate::support::{PgFixture, Ran, pg_fixture, run, text};
use dbento_core::{ExecuteError, Limits, SessionOptions};
use dbento_testkit::Listener;

const SETUP: &str = "CREATE TABLE t(a int); INSERT INTO t VALUES (1), (2); CREATE SEQUENCE s; \
    CREATE PROCEDURE p() LANGUAGE plpgsql AS $$ BEGIN COMMIT; DELETE FROM t; END $$";
const LOCKS: &str = "SELECT count(*) FROM pg_locks WHERE locktype = 'advisory' \
    AND database = (SELECT oid FROM pg_database WHERE datname = current_database())";

struct Escape {
    fx: PgFixture,
    listener: Listener,
}

async fn escape() -> Escape {
    let fx = pg_fixture("read_only = true\nstatement_timeout = \"30s\"").await;
    fx.db.admin().await.batch_execute(SETUP).await.unwrap();
    let listener = fx.db.listen("dbento_escape").await;
    Escape { fx, listener }
}

/// Runs `sql` on a new read-only session and checks that it left nothing behind; returns the
/// outcome codes: "ok", a SQLSTATE, "read_only", "transaction_control" or "unsupported".
async fn attempt(e: &mut Escape, sql: &str) -> Vec<String> {
    let mut s = e.fx.session(SessionOptions::default()).await;
    let results = run(&mut s, sql, Limits::default()).await;
    assert_eq!(
        e.fx.db.scalar("SELECT count(*) FROM t").await.as_deref(),
        Some("2"),
        "{sql}: rows changed"
    );
    assert_eq!(
        e.listener.received(Duration::from_millis(300)).await,
        Vec::<String>::new(),
        "{sql}: a notification escaped"
    );
    let r = run(&mut s, "SHOW statement_timeout", Limits::default()).await;
    assert_eq!(
        r[0].as_ref().unwrap().rows,
        vec![vec![text("30s")]],
        "{sql}: the timeout changed"
    );
    assert_eq!(
        e.fx.db.scalar(LOCKS).await.as_deref(),
        Some("0"),
        "{sql}: an advisory lock survived"
    );
    results.iter().map(code).collect()
}

fn code(r: &Result<Ran, ExecuteError>) -> String {
    match r {
        Ok(_) => "ok".to_owned(),
        Err(ExecuteError::Database { code, .. }) => code.clone(),
        Err(ExecuteError::ReadOnlyViolation { .. }) => "read_only".to_owned(),
        Err(ExecuteError::Unsupported { .. }) => "unsupported".to_owned(),
        Err(ExecuteError::TransactionControl { .. }) => "transaction_control".to_owned(),
        Err(other) => format!("{other:?}"),
    }
}

#[tokio::test]
async fn the_listener_hears_a_read_write_notify() {
    let mut e = escape().await;
    e.fx.db
        .admin()
        .await
        .batch_execute("NOTIFY dbento_escape, 'heard'")
        .await
        .unwrap();
    assert_eq!(
        e.listener.received(Duration::from_millis(300)).await,
        vec!["heard".to_owned()]
    );
}

#[tokio::test]
async fn switching_the_transaction_to_read_write_fails() {
    let mut e = escape().await;
    assert_eq!(
        attempt(&mut e, "SET TRANSACTION READ WRITE; DELETE FROM t").await,
        ["transaction_control"]
    );
    assert_eq!(
        attempt(
            &mut e,
            "SELECT set_config('transaction_read_only', 'off', true); DELETE FROM t"
        )
        .await,
        ["25001"]
    );
    assert_eq!(
        attempt(&mut e, "SET transaction_read_only = off; DELETE FROM t").await,
        ["25001"]
    );
    assert_eq!(
        attempt(
            &mut e,
            "DO $$ BEGIN EXECUTE 'SET TRANSACTION READ WRITE'; DELETE FROM t; END $$"
        )
        .await,
        ["25001"]
    );
}

#[tokio::test]
async fn committing_out_of_the_wrapper_fails() {
    let mut e = escape().await;
    assert_eq!(
        attempt(
            &mut e,
            "SET SESSION CHARACTERISTICS AS TRANSACTION READ WRITE; COMMIT; DELETE FROM t"
        )
        .await,
        ["transaction_control"]
    );
    assert_eq!(
        attempt(&mut e, "COMMIT; DELETE FROM t").await,
        ["transaction_control"]
    );
    assert_eq!(attempt(&mut e, "CALL p()").await, ["2D000"]);
    assert_eq!(
        attempt(&mut e, "DO $$ BEGIN COMMIT; END $$").await,
        ["2D000"]
    );
}

#[tokio::test]
async fn hidden_writes_are_read_only_violations() {
    let mut e = escape().await;
    assert_eq!(
        attempt(
            &mut e,
            "WITH d AS (DELETE FROM t RETURNING *) SELECT count(*) FROM d"
        )
        .await,
        ["read_only"]
    );
    assert_eq!(
        attempt(&mut e, "SELECT setval('s', 100)").await,
        ["read_only"]
    );
}

#[tokio::test]
async fn notifications_are_rolled_back() {
    let mut e = escape().await;
    assert_eq!(attempt(&mut e, "NOTIFY dbento_escape, 'n1'").await, ["ok"]);
    assert_eq!(
        attempt(&mut e, "SELECT pg_notify('dbento_escape', 'n2')").await,
        ["ok"]
    );
}

#[tokio::test]
async fn the_statement_timeout_cannot_be_changed() {
    let mut e = escape().await;
    assert_eq!(attempt(&mut e, "SET statement_timeout = 0").await, ["ok"]);
    assert_eq!(
        attempt(&mut e, "SELECT set_config('statement_timeout', '0', false)").await,
        ["ok"]
    );
    assert_eq!(attempt(&mut e, "RESET statement_timeout").await, ["ok"]);
}

#[tokio::test]
async fn session_advisory_locks_are_released() {
    let mut e = escape().await;
    assert_eq!(
        attempt(
            &mut e,
            "SELECT pg_advisory_lock(42); SELECT pg_advisory_lock_shared(43)"
        )
        .await,
        ["ok", "ok"]
    );
}

#[tokio::test]
async fn a_statement_pg_query_cannot_parse_still_runs_inside_the_wrapper() {
    let mut e = escape().await;
    let codes = attempt(&mut e, "CREATE TABLE v AS SELECT a FROM t GROUP BY ALL").await;
    assert!(codes == ["42601"] || codes == ["read_only"], "{codes:?}");
    assert_eq!(
        e.fx.db
            .scalar("SELECT to_regclass('v') IS NULL")
            .await
            .as_deref(),
        Some("t")
    );
}
