//! Read-only Postgres sessions: the engine refuses writes, and each statement is its own
//! read-only transaction that has ended by the time its result is reported.
mod support;

use std::future::Future;
use std::time::Duration;

use dbento_core::{ExecuteError, Limits, SessionOptions};
use support::{collect, pg_fixture, run, text};

const STATE: &str = "SELECT state FROM pg_stat_activity WHERE application_name = 'dbento' AND datname = current_database()";

fn read_only() -> SessionOptions {
    let mut opts = SessionOptions::default();
    opts.force_read_only = true;
    opts
}

/// A wedged wrapper must fail the test, not hang the suite.
async fn bounded(test: impl Future<Output = ()>) {
    tokio::time::timeout(Duration::from_secs(120), test)
        .await
        .expect("test timed out");
}

#[tokio::test]
async fn a_read_only_connection_reads_inside_a_read_only_transaction() {
    bounded(async {
        let fx = pg_fixture("read_only = true").await;
        let mut s = fx.session(SessionOptions::default()).await;
        assert!(s.is_read_only());
        let r = run(
            &mut s,
            "SELECT 1; SHOW transaction_read_only",
            Limits::default(),
        )
        .await;
        assert_eq!(r[0].as_ref().unwrap().rows, vec![vec![text("1")]]);
        assert_eq!(r[1].as_ref().unwrap().rows, vec![vec![text("on")]]);
        assert_eq!(fx.db.scalar(STATE).await.as_deref(), Some("idle"));
    })
    .await;
}

#[tokio::test]
async fn writes_are_read_only_violations_that_change_nothing() {
    bounded(async {
        let fx = pg_fixture("").await;
        fx.db
            .admin()
            .await
            .batch_execute(
                "CREATE TABLE t(a int); INSERT INTO t VALUES (1), (2); CREATE SEQUENCE s",
            )
            .await
            .unwrap();
        let mut s = fx.session(read_only()).await;
        for sql in [
            "DELETE FROM t WHERE a = 1",
            "WITH d AS (DELETE FROM t RETURNING *) SELECT count(*) FROM d",
            "INSERT INTO t VALUES (3)",
            "CREATE TEMP TABLE x(a int)",
            "SELECT nextval('s')",
        ] {
            let r = run(&mut s, sql, Limits::default()).await;
            assert!(
                matches!(r[0], Err(ExecuteError::ReadOnlyViolation { .. })),
                "{sql}: {:?}",
                r[0].as_ref().err()
            );
            assert_eq!(fx.db.scalar(STATE).await.as_deref(), Some("idle"), "{sql}");
        }
        assert_eq!(
            fx.db.scalar("SELECT count(*) FROM t").await.as_deref(),
            Some("2")
        );
        let r = run(&mut s, "SELECT count(*) FROM t", Limits::default()).await;
        assert_eq!(r[0].as_ref().unwrap().rows, vec![vec![text("2")]]);
    })
    .await;
}

#[tokio::test]
async fn transaction_control_is_refused_and_ends_the_script() {
    bounded(async {
        let fx = pg_fixture("read_only = true").await;
        let mut s = fx.session(SessionOptions::default()).await;
        for sql in [
            "BEGIN",
            "START TRANSACTION READ WRITE",
            "COMMIT",
            "ROLLBACK",
            "SAVEPOINT a",
            "SET TRANSACTION READ WRITE",
            "SET SESSION CHARACTERISTICS AS TRANSACTION READ WRITE",
        ] {
            let r = run(
                &mut s,
                &format!("{sql}; SELECT 'not reached'"),
                Limits::default(),
            )
            .await;
            assert_eq!(r.len(), 1, "{sql}");
            assert!(
                matches!(r[0], Err(ExecuteError::TransactionControl { .. })),
                "{sql}: {:?}",
                r[0].as_ref().err()
            );
        }
        let r = run(&mut s, "SELECT 'still here'", Limits::default()).await;
        assert_eq!(r[0].as_ref().unwrap().rows, vec![vec![text("still here")]]);
    })
    .await;
}

#[tokio::test]
async fn a_read_write_session_keeps_transaction_control() {
    bounded(async {
        let fx = pg_fixture("").await;
        fx.db
            .admin()
            .await
            .batch_execute("CREATE TABLE t(a int)")
            .await
            .unwrap();
        let mut s = fx.session(SessionOptions::default()).await;
        let r = run(
            &mut s,
            "BEGIN; INSERT INTO t VALUES (1); ROLLBACK; SELECT count(*) FROM t",
            Limits::default(),
        )
        .await;
        assert_eq!(r.len(), 4);
        assert_eq!(r[3].as_ref().unwrap().rows, vec![vec![text("0")]]);
    })
    .await;
}

#[tokio::test]
async fn a_failed_prepare_ends_the_wrapper_and_the_script() {
    bounded(async {
        let fx = pg_fixture("read_only = true").await;
        let mut s = fx.session(SessionOptions::default()).await;
        let r = run(&mut s, "SELEC 1; SELECT 2", Limits::default()).await;
        assert_eq!(r.len(), 1);
        assert!(
            matches!(r[0], Err(ExecuteError::Database { ref code, .. }) if code == "42601"),
            "{:?}",
            r[0].as_ref().err()
        );
        assert_eq!(fx.db.scalar(STATE).await.as_deref(), Some("idle"));
        let r = run(&mut s, "SELECT 2", Limits::default()).await;
        assert_eq!(r[0].as_ref().unwrap().rows, vec![vec![text("2")]]);
    })
    .await;
}

#[tokio::test]
async fn a_skipped_or_truncated_statement_ends_its_wrapper_at_the_next_request() {
    bounded(async {
        let fx = pg_fixture("read_only = true").await;
        let mut s = fx.session(SessionOptions::default()).await;
        {
            let mut script = s.script("SELECT 1; SELECT 2");
            script.next().await.unwrap().unwrap().skip();
            assert_eq!(
                fx.db.scalar(STATE).await.as_deref(),
                Some("idle in transaction")
            );
            let p = script.next().await.unwrap().unwrap();
            assert_eq!(
                collect(p, Limits::default()).await.unwrap().rows,
                vec![vec![text("2")]]
            );
        }
        let mut limits = Limits::default();
        limits.max_rows = Some(3);
        let r = run(
            &mut s,
            "SELECT g FROM generate_series(1, 100000) g; SHOW transaction_read_only",
            limits,
        )
        .await;
        let first = r[0].as_ref().unwrap();
        assert_eq!((first.outcome.rows, first.outcome.truncated), (3, true));
        assert_eq!(r[1].as_ref().unwrap().rows, vec![vec![text("on")]]);
        assert_eq!(fx.db.scalar(STATE).await.as_deref(), Some("idle"));
    })
    .await;
}

#[tokio::test]
async fn a_read_write_session_flags_transaction_control_on_the_pending_statement() {
    bounded(async {
        let fx = pg_fixture("").await;
        let mut s = fx.session(SessionOptions::default()).await;
        for (sql, tc) in [
            ("BEGIN", true),
            ("SET TRANSACTION READ WRITE", true),
            ("SELECT 1", false),
        ] {
            let mut script = s.script(sql);
            let pending = script.next().await.unwrap().unwrap();
            assert_eq!(pending.classification().transaction_control, tc, "{sql}");
            pending.skip();
        }
    })
    .await;
}
