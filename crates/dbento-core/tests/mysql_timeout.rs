//! The statement timeout on MySQL and MariaDB: the watchdog stops any statement at the
//! deadline, MariaDB's max_statement_time backs it, and an ended statement's watchdog is gone.
use crate::support;
use std::time::{Duration, Instant};

use crate::support::{Later, my_fixtures, run, text};
use dbento_core::{ExecuteError, Limits, OnTruncate, SessionOptions};
use dbento_testkit::{MyFlavor, NUMS_SQL};
use mysql_async::prelude::*;

fn timed_out(r: &[Result<support::Ran, ExecuteError>]) -> bool {
    matches!(r.last(), Some(Err(ExecuteError::Timeout)))
}

#[tokio::test]
async fn a_statement_past_the_timeout_times_out_and_the_session_goes_on() {
    for (label, fx) in my_fixtures("statement_timeout = \"1s\"").await {
        let mut admin = fx.db.admin().await;
        admin.query_drop(NUMS_SQL).await.unwrap();
        admin
            .query_drop("CREATE TABLE t (a INT) ENGINE = InnoDB")
            .await
            .unwrap();
        let mut s = fx.session(SessionOptions::default()).await;
        for sql in ["SELECT SLEEP(5)", "DO SLEEP(5)"] {
            let started = Instant::now();
            let r = run(&mut s, sql, Limits::default()).await;
            assert!(timed_out(&r), "{label}: {sql}");
            assert!(
                started.elapsed() < Duration::from_secs(4),
                "{label}: {sql}: {:?}",
                started.elapsed()
            );
        }
        // A write that MySQL's max_execution_time would not bound: about 1,000 rows a second.
        // Its rollback takes as long as it takes, so it has no time bound.
        let write = "INSERT INTO t SELECT a.n FROM nums a, nums b WHERE SLEEP(0.001) = 0";
        assert!(
            timed_out(&run(&mut s, write, Limits::default()).await),
            "{label}"
        );
        assert_eq!(
            fx.db.scalar("SELECT COUNT(*) FROM t").await.as_deref(),
            Some("0"),
            "{label}"
        );
        let after = run(&mut s, "SELECT 7", Limits::default())
            .await
            .remove(0)
            .unwrap();
        assert_eq!(after.rows, vec![vec![text("7")]], "{label}");
    }
}

#[tokio::test]
async fn mariadb_also_bounds_the_session_and_mysql_leaves_max_execution_time() {
    for (label, fx) in my_fixtures("").await {
        let mut opts = SessionOptions::default();
        opts.statement_timeout = Some(Duration::from_millis(1500));
        let mut s = fx.session(opts).await;
        let sql = match fx.db.flavor() {
            MyFlavor::Mariadb => "SELECT @@SESSION.max_statement_time = 1.5",
            MyFlavor::Mysql => "SELECT @@SESSION.max_execution_time = 0",
        };
        let ran = run(&mut s, sql, Limits::default()).await.remove(0).unwrap();
        assert_eq!(ran.rows, vec![vec![text("1")]], "{label}");
    }
}

#[tokio::test]
async fn a_finished_statements_watchdog_never_stops_the_next_one() {
    for (label, fx) in my_fixtures("statement_timeout = \"1s\"").await {
        let mut s = fx.session(SessionOptions::default()).await;
        assert!(
            run(&mut s, "SELECT 1", Limits::default())
                .await
                .remove(0)
                .is_ok(),
            "{label}"
        );
        tokio::time::sleep(Duration::from_millis(700)).await;
        // A leftover watchdog from the first statement would fire 0.3 s into this one.
        let r = run(&mut s, "SELECT SLEEP(0.6)", Limits::default())
            .await
            .remove(0);
        assert_eq!(r.unwrap().rows, vec![vec![text("0")]], "{label}");
    }
}

#[tokio::test]
async fn a_parked_result_past_the_timeout_times_out_when_resumed() {
    for (label, fx) in my_fixtures("statement_timeout = \"1s\"").await {
        fx.db.admin().await.query_drop(NUMS_SQL).await.unwrap();
        let mut s = fx.session(SessionOptions::default()).await;
        let mut limits = Limits::default();
        limits.max_rows = Some(10);
        limits.on_truncate = OnTruncate::Park;
        let mut script = s.script("SELECT a.n * 1000 + b.n FROM nums a, nums b");
        let mut stream = script
            .next()
            .await
            .unwrap()
            .unwrap()
            .execute(limits)
            .await
            .unwrap();
        while stream.next_batch().await.unwrap().is_some() {}
        assert!(stream.outcome().unwrap().truncated, "{label}");
        tokio::time::sleep(Duration::from_millis(1500)).await;
        stream.extend(None).await.unwrap();
        let ended = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match stream.next_batch().await {
                    Ok(Some(_)) => continue,
                    other => break other.map(|_| ()),
                }
            }
        })
        .await
        .expect("the parked statement ran on");
        assert!(
            matches!(ended, Err(ExecuteError::Timeout)),
            "{label}: {ended:?}"
        );
        // Settled here: the fixture's blocking DROP DATABASE would otherwise wait on this
        // statement's lock while the runtime that severs it is blocked.
        drop(stream);
        drop(script);
        assert!(s.settle().await.is_ok(), "{label}");
    }
}

#[tokio::test]
async fn a_timeout_whose_kill_cannot_connect_closes_the_session_at_once() {
    for (label, fx) in my_fixtures("").await {
        fx.db.admin().await.query_drop(NUMS_SQL).await.unwrap();
        // `proxied` rewrites the connection without extra TOML, so the timeout is an option.
        let fx = fx.proxied(Later::Refuse).await;
        let mut opts = SessionOptions::default();
        opts.statement_timeout = Some(Duration::from_secs(1));
        let mut s = fx.session(opts).await;
        let mut limits = Limits::default();
        limits.max_rows = None;
        let sql = "SELECT a.n FROM nums a, nums b, nums c";
        let first = tokio::time::timeout(Duration::from_secs(20), run(&mut s, sql, limits))
            .await
            .expect("the watchdog never fired");
        assert!(timed_out(&first), "{label}");
        // The unreachable KILL leaves a drain only the 10 s cap would end; it is severed instead.
        let started = Instant::now();
        let next = run(&mut s, "SELECT 1", Limits::default()).await.remove(0);
        assert!(matches!(next, Err(ExecuteError::SessionClosed)), "{label}");
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{label}: waited {:?}",
            started.elapsed()
        );
    }
}
