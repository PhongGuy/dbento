//! Cancel on MySQL and MariaDB: KILL QUERY from a side connection stops exactly the statement
//! it was aimed at, and a cancel between prepare and execute stops the statement unsent.
mod support;
use std::time::Duration;

use dbento_core::{CancelHandle, ExecuteError, Limits, SessionOptions};
use dbento_testkit::NUMS_SQL;
use mysql_async::prelude::*;
use support::{Later, my_fixtures, run, text};

fn cancel_after(cancel: CancelHandle, after: Duration) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        std::thread::sleep(after);
        cancel.cancel();
    })
}

#[tokio::test]
async fn a_cancel_stops_the_running_statement_and_the_session_goes_on() {
    for (label, fx) in my_fixtures("").await {
        fx.db.admin().await.query_drop(NUMS_SQL).await.unwrap();
        let mut s = fx.session(SessionOptions::default()).await;
        // A sleep that returns 1 when killed, one without columns, a stream that ends in 1317.
        for sql in [
            "SELECT SLEEP(30)",
            "DO SLEEP(30)",
            "SELECT a.n FROM nums a, nums b, nums c",
        ] {
            let t = cancel_after(s.cancel_handle(), Duration::from_millis(300));
            let r =
                tokio::time::timeout(Duration::from_secs(20), run(&mut s, sql, Limits::default()))
                    .await
                    .expect("the cancel never arrived");
            t.join().unwrap();
            assert!(
                matches!(r.last(), Some(Err(ExecuteError::Cancelled))),
                "{label}: {sql}: {:?}",
                r.last().map(|r| r.as_ref().err())
            );
            let after = run(&mut s, "SELECT 7", Limits::default())
                .await
                .remove(0)
                .unwrap();
            assert_eq!(after.rows, vec![vec![text("7")]], "{label}: {sql}");
        }
    }
}

#[tokio::test]
async fn a_cancel_after_prepare_stops_the_statement_before_it_is_sent() {
    for (label, fx) in my_fixtures("").await {
        fx.db
            .admin()
            .await
            .query_drop("CREATE TABLE t (a INT)")
            .await
            .unwrap();
        let mut s = fx.session(SessionOptions::default()).await;
        let cancel = s.cancel_handle();
        {
            let mut script = s.script("INSERT INTO t VALUES (1)");
            let pending = script.next().await.unwrap().unwrap();
            cancel.cancel();
            let sent = pending.execute(Limits::default()).await.err();
            assert!(matches!(sent, Some(ExecuteError::Cancelled)), "{label}");
        }
        assert_eq!(
            fx.db.scalar("SELECT COUNT(*) FROM t").await.as_deref(),
            Some("0"),
            "{label}"
        );
        let again = run(&mut s, "INSERT INTO t VALUES (2)", Limits::default())
            .await
            .remove(0);
        assert!(again.is_ok(), "{label}: the cancel was not spent");
        assert_eq!(
            fx.db.scalar("SELECT COUNT(*) FROM t").await.as_deref(),
            Some("1"),
            "{label}"
        );
    }
}

#[tokio::test]
async fn a_late_kill_never_stops_the_next_statement() {
    for (label, fx) in my_fixtures("").await {
        // Side connections reach the server 150 ms late, after the first statement ended.
        let fx = fx.proxied(Later::Delay(Duration::from_millis(150))).await;
        let mut s = fx.session(SessionOptions::default()).await;
        for round in 0..5 {
            let t = cancel_after(s.cancel_handle(), Duration::from_millis(30));
            let first = run(&mut s, "SELECT SLEEP(0.1)", Limits::default())
                .await
                .remove(0);
            t.join().unwrap();
            assert!(
                matches!(first, Err(ExecuteError::Cancelled)),
                "{label} {round}: {:?}",
                first.err()
            );
            let second = run(&mut s, "SELECT SLEEP(0.3)", Limits::default())
                .await
                .remove(0);
            assert_eq!(
                second.unwrap().rows,
                vec![vec![text("0")]],
                "{label} round {round}"
            );
        }
    }
}
