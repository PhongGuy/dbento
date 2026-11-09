//! Client cancellation and statement timeouts on Postgres.
use std::time::{Duration, Instant};

use crate::common::TestServer;
use dbento_core::{History, HistoryFilter, HistoryOutcome};
use dbento_testkit::{PgDb, pg_db};
use serde_json::json;

/// Counts this test database's running `SELECT pg_sleep(60)` statements.
fn active_sleep(db: &PgDb) -> String {
    format!(
        "SELECT count(*) FROM pg_stat_activity WHERE state = 'active' AND query = 'SELECT pg_sleep(60)' AND datname = '{}'",
        db.database()
    )
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
async fn a_client_cancel_stops_the_statement_in_the_database() {
    let db = pg_db().await;
    let server = TestServer::postgres(&db, "").await;
    let handle = server
        .start_call(
            "query",
            json!({"connection": "pg", "sql": "SELECT pg_sleep(60)"}),
        )
        .await;
    assert!(
        until(&db, &active_sleep(&db), "1").await,
        "the sleep never started"
    );
    handle.cancel(None).await.expect("cancel sent");
    assert!(
        until(&db, &active_sleep(&db), "0").await,
        "the sleep still runs after the cancel"
    );
    let mut recorded = false;
    for _ in 0..50 {
        let entries = History::open(&server.history_file())
            .unwrap()
            .recent(&HistoryFilter::default(), 10);
        if entries
            .iter()
            .any(|e| e.sql() == "SELECT pg_sleep(60)" && *e.outcome() == HistoryOutcome::Cancelled)
        {
            recorded = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(recorded, "no Cancelled history entry");
}

/// Runs `SELECT pg_sleep(5)` and requires a `timeout` error well before it would finish.
async fn times_out(server: &TestServer) {
    let started = Instant::now();
    let e = server
        .err(
            "query",
            json!({"connection": "pg", "sql": "SELECT pg_sleep(5)"}),
        )
        .await;
    assert_eq!(e["code"], "timeout", "{e}");
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn the_shorter_of_the_flag_and_the_connection_timeout_stops_a_statement() {
    let db = pg_db().await;
    times_out(&TestServer::postgres_with(&db, "", |o| o.timeout(Duration::from_secs(1))).await)
        .await;
    // The connection's own shorter timeout wins over the 30 s default.
    times_out(&TestServer::postgres(&db, "statement_timeout = \"300ms\"").await).await;
    // The flag wins over a longer connection timeout.
    times_out(
        &TestServer::postgres_with(&db, "statement_timeout = \"10s\"", |o| {
            o.timeout(Duration::from_secs(1))
        })
        .await,
    )
    .await;
}
