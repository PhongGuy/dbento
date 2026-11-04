//! `Session::explain` on Postgres: every block it opens is rolled back, on every path.
mod support;
use std::future::Future;
use std::task::Poll;
use std::time::{Duration, Instant};

use dbento_core::{
    Engine, ExecuteError, ExplainMode, ExplainRequest, Limits, Session, SessionOptions,
    explain_request,
};
use support::{Cell, pg_fixture, run, text};

const SETUP: &str =
    "CREATE TABLE t(id serial PRIMARY KEY, a int); INSERT INTO t(a) VALUES (1), (2);";
const LOCKS: &str = "SELECT count(*)::text FROM pg_locks WHERE locktype = 'advisory'";

async fn bounded(test: impl Future<Output = ()>) {
    tokio::time::timeout(Duration::from_secs(120), test)
        .await
        .expect("test timed out");
}
fn read_only() -> SessionOptions {
    let mut opts = SessionOptions::default();
    opts.force_read_only = true;
    opts
}
fn req(sql: &str, mode: ExplainMode) -> ExplainRequest {
    explain_request(Engine::Postgres, sql, mode).unwrap()
}
/// "false" inside a transaction block, where now() is the block's start.
async fn outside_block(s: &mut Session) -> String {
    let r = run(
        s,
        "SELECT (now() = statement_timestamp())::text",
        Limits::default(),
    )
    .await
    .remove(0)
    .unwrap();
    match &r.rows[0][0] {
        Cell::Text(t) => t.clone(),
        other => panic!("{other:?}"),
    }
}
async fn count_in(s: &mut Session) -> Vec<Vec<Cell>> {
    run(s, "SELECT count(*)::text FROM t", Limits::default())
        .await
        .remove(0)
        .unwrap()
        .rows
}

#[tokio::test]
async fn analyze_of_an_insert_is_rolled_back_but_its_sequence_moves() {
    bounded(async {
        let fx = pg_fixture("").await;
        fx.db.admin().await.batch_execute(SETUP).await.unwrap();
        let mut s = fx.session(SessionOptions::default()).await;
        let plain = s
            .explain(&req("SELECT * FROM t WHERE id = 1", ExplainMode::Plan))
            .await
            .unwrap();
        assert!(plain.raw.trim_start().starts_with('[') && !plain.plan.unwrap().analyzed);
        let out = s
            .explain(&req("INSERT INTO t(a) VALUES (3)", ExplainMode::Analyze))
            .await
            .unwrap();
        assert!(out.plan.unwrap().analyzed);
        assert_eq!(
            fx.db
                .scalar("SELECT count(*)::text FROM t")
                .await
                .as_deref(),
            Some("2")
        );
        assert_eq!(
            fx.db
                .scalar("SELECT last_value::text FROM t_id_seq")
                .await
                .as_deref(),
            Some("3")
        );
        assert_eq!(outside_block(&mut s).await, "true");
    })
    .await;
}

#[tokio::test]
async fn a_read_only_session_refuses_the_write_and_keeps_no_lock() {
    bounded(async {
        let fx = pg_fixture("").await;
        fx.db.admin().await.batch_execute(SETUP).await.unwrap();
        let mut s = fx.session(read_only()).await;
        let e = s
            .explain(&req("INSERT INTO t(a) VALUES (3)", ExplainMode::Analyze))
            .await
            .unwrap_err();
        assert!(matches!(e, ExecuteError::ReadOnlyViolation { .. }), "{e:?}");
        assert_eq!(
            fx.db
                .scalar("SELECT count(*)::text FROM t")
                .await
                .as_deref(),
            Some("2")
        );
        s.explain(&req("SELECT pg_advisory_lock(1)", ExplainMode::Analyze))
            .await
            .unwrap();
        assert_eq!(fx.db.scalar(LOCKS).await.as_deref(), Some("0"));
        // A writable session keeps a session-level lock, as a normal run would.
        let mut w = fx.session(SessionOptions::default()).await;
        w.explain(&req("SELECT pg_advisory_lock(2)", ExplainMode::Analyze))
            .await
            .unwrap();
        assert_eq!(fx.db.scalar(LOCKS).await.as_deref(), Some("1"));
    })
    .await;
}

#[tokio::test]
async fn inside_the_users_block_only_the_savepoint_is_rolled_back() {
    bounded(async {
        let fx = pg_fixture("").await;
        fx.db.admin().await.batch_execute(SETUP).await.unwrap();
        let mut s = fx.session(SessionOptions::default()).await;
        for r in run(
            &mut s,
            "BEGIN; INSERT INTO t(a) VALUES (10)",
            Limits::default(),
        )
        .await
        {
            r.unwrap();
        }
        s.explain(&req("DELETE FROM t", ExplainMode::Analyze))
            .await
            .unwrap();
        assert_eq!(outside_block(&mut s).await, "false");
        assert_eq!(count_in(&mut s).await, vec![vec![text("3")]]);
        // A cancel lands on the SAVEPOINT or the request; either way the block survives.
        let cancel = s.cancel_handle();
        let canceller = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            cancel.cancel();
        });
        let e = s
            .explain(&req("SELECT pg_sleep(30)", ExplainMode::Analyze))
            .await
            .unwrap_err();
        canceller.await.unwrap();
        assert!(matches!(e, ExecuteError::Cancelled), "{e:?}");
        assert_eq!(outside_block(&mut s).await, "false");
        assert_eq!(count_in(&mut s).await, vec![vec![text("3")]]);
        for r in run(&mut s, "ROLLBACK", Limits::default()).await {
            r.unwrap();
        }
        assert_eq!(
            fx.db
                .scalar("SELECT count(*)::text FROM t")
                .await
                .as_deref(),
            Some("2")
        );
    })
    .await;
}

#[tokio::test]
async fn a_dropped_explain_is_cancelled_and_rolled_back_before_the_next_statement() {
    bounded(async {
        let fx = pg_fixture("").await;
        fx.db.admin().await.batch_execute(SETUP).await.unwrap();
        let mut s = fx.session(SessionOptions::default()).await;
        let slow = req(
            "INSERT INTO t(a) SELECT 99 FROM pg_sleep(30)",
            ExplainMode::Analyze,
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(300), s.explain(&slow))
                .await
                .is_err()
        );
        let started = Instant::now();
        assert_eq!(outside_block(&mut s).await, "true");
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "the dropped ANALYZE was waited out"
        );
        assert_eq!(
            fx.db
                .scalar("SELECT count(*)::text FROM t")
                .await
                .as_deref(),
            Some("2")
        );
        // Inside the user's block a dropped explain rolls back to its savepoint only.
        for r in run(
            &mut s,
            "BEGIN; INSERT INTO t(a) VALUES (10)",
            Limits::default(),
        )
        .await
        {
            r.unwrap();
        }
        let slow = req(
            "DELETE FROM t WHERE (SELECT true FROM pg_sleep(30))",
            ExplainMode::Analyze,
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(300), s.explain(&slow))
                .await
                .is_err()
        );
        assert_eq!(outside_block(&mut s).await, "false");
        assert_eq!(count_in(&mut s).await, vec![vec![text("3")]]);
        for r in run(&mut s, "ROLLBACK", Limits::default()).await {
            r.unwrap();
        }
    })
    .await;
}

#[tokio::test]
async fn a_settle_dropped_while_it_cancels_still_owes_the_close() {
    bounded(async {
        let fx = pg_fixture("").await;
        fx.db.admin().await.batch_execute(SETUP).await.unwrap();
        let mut s = fx.session(SessionOptions::default()).await;
        let pid = run(&mut s, "SELECT pg_backend_pid()::text", Limits::default())
            .await
            .remove(0)
            .unwrap();
        let Cell::Text(pid) = &pid.rows[0][0] else {
            panic!("{:?}", pid.rows)
        };
        let state = format!("SELECT state FROM pg_stat_activity WHERE pid = {pid}");
        let slow = req(
            "INSERT INTO t(a) SELECT 99 FROM pg_sleep(30)",
            ExplainMode::Analyze,
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(300), s.explain(&slow))
                .await
                .is_err()
        );
        assert_eq!(fx.db.scalar(&state).await.as_deref(), Some("active"));
        // The next run is polled once, which sends a cancel for the dropped ANALYZE and waits
        // for it to land. It is not polled again: once the backend leaves `active` the cancel
        // has landed, the settle is still parked before the close, and it is dropped there.
        {
            let mut next = std::pin::pin!(outside_block(&mut s));
            let first = std::future::poll_fn(|cx| Poll::Ready(next.as_mut().poll(cx))).await;
            assert!(first.is_pending());
            let deadline = Instant::now() + Duration::from_secs(10);
            while fx.db.scalar(&state).await.as_deref() == Some("active") {
                assert!(
                    Instant::now() < deadline,
                    "the first poll never sent its cancel"
                );
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
        assert_eq!(outside_block(&mut s).await, "true");
        assert_eq!(
            fx.db
                .scalar("SELECT count(*)::text FROM t")
                .await
                .as_deref(),
            Some("2")
        );
    })
    .await;
}
