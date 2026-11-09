//! `is_last` tells the last statement apart; `settle` leaves a session idle, reports a
//! drain error, and is a barrier on SQLite.
use std::time::Duration;

use crate::support::{PgFixture, collect, pg_fixture, run, sqlite_fixture};
use dbento_core::{ExecuteError, Limits, Session, SessionOptions};

/// `is_last` for each statement of `sql`, asked before it runs; each runs to its end.
async fn lasts(s: &mut Session, sql: &str) -> Vec<bool> {
    let mut out = Vec::new();
    let mut script = s.script(sql);
    while let Some(p) = script.next().await {
        let mut p = p.expect("prepares");
        out.push(p.is_last().await);
        let mut stream = p.execute(Limits::default()).await.expect("runs");
        while stream.next_batch().await.expect("streams").is_some() {}
    }
    out
}

async fn our_state(fx: &PgFixture) -> Option<String> {
    fx.db
        .scalar(
            "SELECT state FROM pg_stat_activity \
             WHERE application_name = 'dbento' AND datname = current_database()",
        )
        .await
}

fn capped(n: u64) -> Limits {
    let mut l = Limits::default();
    l.max_rows = Some(n);
    l
}

const DIV: &str = "SELECT 1 / (x - 3000) FROM generate_series(1, 5000) x";
/// Far larger than the socket buffers, so an undrained result leaves the backend `active`.
const WIDE: &str = "SELECT repeat('x', 1000) FROM generate_series(1, 100000)";

#[tokio::test]
async fn both_engines_know_the_last_statement() {
    let fx = sqlite_fixture("CREATE TABLE t(x);", "");
    let mut s = fx.session(SessionOptions::default()).await;
    assert_eq!(lasts(&mut s, "SELECT 1; SELECT 2").await, [false, true]);
    assert_eq!(lasts(&mut s, "SELECT 1; -- trailing\n  ").await, [true]);
    assert_eq!(lasts(&mut s, "SELECT 1;;").await, [true]);
    // The INSERT cannot be prepared before the CREATE runs, so it still counts as following.
    assert_eq!(
        lasts(
            &mut s,
            "CREATE TABLE n(x); INSERT INTO n VALUES (1); SELECT x FROM n"
        )
        .await,
        [false, false, true]
    );
    assert_eq!(
        lasts(
            &mut s,
            "CREATE TRIGGER tr AFTER INSERT ON t BEGIN UPDATE t SET x = 1; UPDATE t SET x = 2; END; SELECT 1"
        )
        .await,
        [false, true]
    );
    s.close().await.unwrap();
    let pg = pg_fixture("").await;
    let mut s = pg.session(SessionOptions::default()).await;
    assert_eq!(lasts(&mut s, "SELECT 1; SELECT 2").await, [false, true]);
    assert_eq!(lasts(&mut s, "SELECT 1; /* end */ ").await, [true]);
    assert_eq!(
        lasts(
            &mut s,
            "CREATE TEMP TABLE n(x int); INSERT INTO n VALUES (1); SELECT x FROM n"
        )
        .await,
        [false, false, true]
    );
    s.close().await.unwrap();
}

#[tokio::test]
async fn settle_drains_a_truncated_result_and_ends_a_wrapper() {
    let fx = pg_fixture("").await;
    for read_only in [false, true] {
        let mut opts = SessionOptions::default();
        opts.force_read_only = read_only;
        let mut s = fx.session(opts).await;
        let ran = run(&mut s, WIDE, capped(10)).await;
        assert!(ran[0].as_ref().unwrap().outcome.truncated);
        s.settle().await.unwrap();
        assert_eq!(
            our_state(&fx).await.as_deref(),
            Some("idle"),
            "read_only {read_only}"
        );
        s.close().await.unwrap();
    }
}

#[tokio::test]
async fn settle_on_a_terminated_session_reports_it_closed() {
    let fx = pg_fixture("").await;
    let mut s = fx.session(SessionOptions::default()).await;
    assert!(run(&mut s, "SELECT 1", Limits::default()).await[0].is_ok());
    fx.db
        .scalar(
            "SELECT count(pg_terminate_backend(pid)) FROM pg_stat_activity \
             WHERE application_name = 'dbento' AND datname = current_database()",
        )
        .await;
    // The client learns of the closed socket on its own task, so poll for it.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let last = loop {
        let r = s.settle().await;
        if r.is_err() || std::time::Instant::now() > deadline {
            break r;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(matches!(last, Err(ExecuteError::SessionClosed)), "{last:?}");
    s.close().await.unwrap();
}

#[tokio::test]
async fn settle_reports_a_drain_error_once() {
    let fx = pg_fixture("").await;
    let mut s = fx.session(SessionOptions::default()).await;
    run(&mut s, DIV, capped(10)).await;
    let err = s.settle().await.unwrap_err();
    assert!(
        matches!(&err, ExecuteError::Database { code, .. } if code == "22012"),
        "{err:?}"
    );
    s.settle().await.unwrap();
    s.settle().await.unwrap();
    assert!(run(&mut s, "SELECT 1", Limits::default()).await[0].is_ok());
    s.close().await.unwrap();
}

#[tokio::test]
async fn a_drain_error_inside_a_script_still_runs_the_next_statement() {
    let fx = pg_fixture("").await;
    let mut s = fx.session(SessionOptions::default()).await;
    let mut script = s.script(&format!("{DIV}; SELECT 'next'"));
    collect(script.next().await.unwrap().unwrap(), capped(10))
        .await
        .unwrap();
    let next = collect(script.next().await.unwrap().unwrap(), Limits::default())
        .await
        .unwrap();
    assert_eq!(next.outcome.rows, 1);
    drop(script);
    s.close().await.unwrap();
}

#[tokio::test]
async fn sqlite_settle_waits_for_an_abandoned_statement_to_let_go() {
    let fx = sqlite_fixture(
        "CREATE TABLE t(x); WITH RECURSIVE r(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM r WHERE x < 5000) INSERT INTO t SELECT x FROM r;",
        "",
    );
    let mut s = fx.session(SessionOptions::default()).await;
    s.settle().await.unwrap(); // idle: a no-op
    {
        let mut script = s.script("SELECT x FROM t");
        let mut stream = script
            .next()
            .await
            .unwrap()
            .unwrap()
            .execute(Limits::default())
            .await
            .unwrap();
        assert!(stream.next_batch().await.unwrap().is_some());
    } // stream and script dropped mid-result
    s.settle().await.unwrap();
    let w = rusqlite::Connection::open(fx.dir.path().join("data/app.db")).unwrap();
    w.busy_timeout(Duration::ZERO).unwrap();
    w.execute("INSERT INTO t VALUES (0)", []).unwrap();
    s.close().await.unwrap();
}
