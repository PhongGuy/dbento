//! Cancel off the happy path: a KILL that cannot connect, a cancel during a settle's drain,
//! and a run dropped after its cancel, as MCP's until_cancelled does.
mod support;
use std::time::{Duration, Instant};

use dbento_core::{ExecuteError, Limits, SessionOptions};
use dbento_testkit::NUMS_SQL;
use mysql_async::prelude::*;
use support::{Later, my_fixtures, my_thread_id, run, text};

#[tokio::test]
async fn a_cancel_whose_kill_cannot_connect_returns_at_once_and_closes_the_session() {
    for (label, fx) in my_fixtures("").await {
        let fx = fx.proxied(Later::Refuse).await;
        let mut s = fx.session(SessionOptions::default()).await;
        let cancel = s.cancel_handle();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            cancel.cancel();
        });
        let started = Instant::now();
        let r = tokio::time::timeout(
            Duration::from_secs(20),
            run(&mut s, "SELECT SLEEP(30)", Limits::default()),
        )
        .await
        .expect("the statement ran on");
        t.join().unwrap();
        assert!(
            matches!(r.last(), Some(Err(ExecuteError::Cancelled))),
            "{label}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{label}: {:?}",
            started.elapsed()
        );
        let next = run(&mut s, "SELECT 1", Limits::default()).await.remove(0);
        assert!(matches!(next, Err(ExecuteError::SessionClosed)), "{label}");
    }
}

#[tokio::test]
async fn a_cancel_during_a_settle_drain_stops_the_drained_statement() {
    for (label, fx) in my_fixtures("").await {
        fx.db.admin().await.query_drop(NUMS_SQL).await.unwrap();
        let mut s = fx.session(SessionOptions::default()).await;
        {
            let mut script = s.script("SELECT a.n FROM nums a, nums b, nums c");
            let pending = script.next().await.unwrap().unwrap();
            let mut stream = pending.execute(Limits::default()).await.unwrap();
            assert!(stream.next_batch().await.unwrap().is_some(), "{label}");
        }
        s.cancel_handle().cancel();
        let started = Instant::now();
        let settled = s.settle().await;
        assert!(
            matches!(settled, Err(ExecuteError::Cancelled)),
            "{label}: {settled:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{label}: waited for the cap"
        );
        let after = run(&mut s, "SELECT 7", Limits::default())
            .await
            .remove(0)
            .unwrap();
        assert_eq!(after.rows, vec![vec![text("7")]], "{label}");
    }
}

#[tokio::test]
async fn a_cancelled_and_dropped_run_stops_the_statement_on_the_server() {
    for (label, fx) in my_fixtures("").await {
        let mut s = fx.session(SessionOptions::default()).await;
        let id = my_thread_id(&mut s).await;
        let cancel = s.cancel_handle();
        {
            let mut work = std::pin::pin!(run(&mut s, "SELECT SLEEP(30)", Limits::default()));
            let early = tokio::time::timeout(Duration::from_millis(500), &mut work).await;
            assert!(early.is_err(), "{label}");
            cancel.cancel();
        }
        assert!(
            fx.quiet_within(&id, Duration::from_secs(5)).await,
            "{label}: the sleep runs on"
        );
        let next = run(&mut s, "SELECT 7", Limits::default()).await.remove(0);
        assert!(
            matches!(&next, Ok(r) if r.rows == vec![vec![text("7")]])
                || matches!(next, Err(ExecuteError::SessionClosed)),
            "{label}"
        );
    }
}

/// Connection options for `fx`'s database, for a client on a runtime of its own.
fn admin_opts(fx: &support::MyFixture) -> mysql_async::OptsBuilder {
    mysql_async::OptsBuilder::default()
        .ip_or_hostname(fx.db.host())
        .tcp_port(fx.db.port())
        .user(Some(fx.db.user()))
        .pass(Some(fx.db.password()))
        .db_name(Some(fx.db.database()))
}

#[tokio::test(flavor = "current_thread")]
async fn a_cancel_stops_the_statement_while_the_session_runtime_is_blocked() {
    for (label, fx) in my_fixtures("").await {
        let mut s = fx.session(SessionOptions::default()).await;
        let id = my_thread_id(&mut s).await;
        let cancel = s.cancel_handle();
        let mut work = std::pin::pin!(run(&mut s, "SELECT SLEEP(30)", Limits::default()));
        let early = tokio::time::timeout(Duration::from_millis(500), &mut work).await;
        assert!(early.is_err(), "{label}");
        // The watcher has a thread and runtime of its own; this runtime's thread then sleeps
        // without awaiting, so nothing on it is polled while the KILL must land.
        let admin = admin_opts(&fx);
        let watcher = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            runtime.block_on(async move {
                let mut conn = mysql_async::Conn::new(admin).await.unwrap();
                let sql = format!(
                    "SELECT COUNT(*) FROM information_schema.PROCESSLIST \
                     WHERE ID = {id} AND INFO IS NOT NULL"
                );
                let deadline = Instant::now() + Duration::from_secs(5);
                let mut gone = false;
                while !gone && Instant::now() < deadline {
                    let busy: Option<u64> = conn.query_first(sql.as_str()).await.unwrap();
                    gone = busy == Some(0);
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                let _ = conn.disconnect().await;
                gone
            })
        });
        cancel.cancel();
        std::thread::sleep(Duration::from_secs(6));
        assert!(
            watcher.join().unwrap(),
            "{label}: the query outlived the blocked runtime"
        );
        let r = tokio::time::timeout(Duration::from_secs(20), &mut work)
            .await
            .expect("the cancel never arrived");
        assert!(
            matches!(r.last(), Some(Err(ExecuteError::Cancelled))),
            "{label}"
        );
    }
}

#[tokio::test]
async fn a_cancelled_hot_stream_returns_at_once_and_the_session_goes_on() {
    for (label, fx) in my_fixtures("").await {
        fx.db.admin().await.query_drop(NUMS_SQL).await.unwrap();
        let mut s = fx.session(SessionOptions::default()).await;
        let cancel = s.cancel_handle();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            cancel.cancel();
            Instant::now()
        });
        let sql = "SELECT a.n FROM nums a, nums b, nums c";
        let r = tokio::time::timeout(Duration::from_secs(20), run(&mut s, sql, Limits::default()))
            .await
            .expect("the cancel never arrived");
        let returned = Instant::now();
        let asked = t.join().unwrap();
        assert!(
            matches!(r.last(), Some(Err(ExecuteError::Cancelled))),
            "{label}"
        );
        assert!(
            returned.duration_since(asked) < Duration::from_millis(1500),
            "{label}: returned {:?} after the cancel",
            returned.duration_since(asked)
        );
        let after = run(&mut s, "SELECT 7", Limits::default())
            .await
            .remove(0)
            .unwrap();
        assert_eq!(after.rows, vec![vec![text("7")]], "{label}");
    }
}

#[tokio::test]
async fn a_kill_from_elsewhere_after_a_cancel_is_reported_as_the_server_error() {
    for (label, fx) in my_fixtures("").await {
        fx.db.admin().await.query_drop(NUMS_SQL).await.unwrap();
        let mut s = fx.session(SessionOptions::default()).await;
        let id = my_thread_id(&mut s).await;
        let cancel = s.cancel_handle();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            cancel.cancel();
        });
        let first = run(&mut s, "SELECT SLEEP(30)", Limits::default()).await;
        t.join().unwrap();
        assert!(
            matches!(first.last(), Some(Err(ExecuteError::Cancelled))),
            "{label}"
        );
        {
            let mut script = s.script("SELECT a.n FROM nums a, nums b, nums c");
            let pending = script.next().await.unwrap().unwrap();
            let mut stream = pending.execute(Limits::default()).await.unwrap();
            assert!(stream.next_batch().await.unwrap().is_some(), "{label}");
        }
        fx.db
            .admin()
            .await
            .query_drop(format!("KILL QUERY {id}"))
            .await
            .unwrap();
        let settled = s.settle().await;
        assert!(
            matches!(&settled, Err(ExecuteError::Database { code, .. }) if code == "1317"),
            "{label}: {settled:?}"
        );
    }
}

#[tokio::test]
async fn a_stream_cancel_whose_kill_cannot_connect_closes_the_session_at_once() {
    for (label, fx) in my_fixtures("").await {
        fx.db.admin().await.query_drop(NUMS_SQL).await.unwrap();
        let fx = fx.proxied(Later::Refuse).await;
        let mut s = fx.session(SessionOptions::default()).await;
        let cancel = s.cancel_handle();
        {
            let mut script = s.script("SELECT a.n FROM nums a, nums b, nums c");
            let pending = script.next().await.unwrap().unwrap();
            let mut stream = pending.execute(Limits::default()).await.unwrap();
            assert!(stream.next_batch().await.unwrap().is_some(), "{label}");
            cancel.cancel();
            let stopped = stream.next_batch().await;
            assert!(matches!(stopped, Err(ExecuteError::Cancelled)), "{label}");
        }
        // The drain the KILL cannot end would run to the 10 s cap; the session is severed instead.
        let started = Instant::now();
        let settled = s.settle().await;
        assert!(
            matches!(settled, Err(ExecuteError::SessionClosed)),
            "{label}: {settled:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "{label}: waited {:?}",
            started.elapsed()
        );
    }
}
