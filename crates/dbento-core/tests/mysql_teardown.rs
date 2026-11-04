//! Every connection the MySQL driver lets go of closes within the drain cap: after a KILL that
//! cannot connect, after a dropped session, and across settles that are cut short.
mod support;
use std::time::{Duration, Instant};

use dbento_core::{ExecuteError, Limits, OnTruncate, SessionOptions};
use dbento_testkit::NUMS_SQL;
use mysql_async::prelude::*;
use support::{Later, my_fixtures, my_thread_id, run};

/// 1,000,000,000 rows: nothing reads or drains it within the cap.
const HUGE: &str = "SELECT a.n FROM nums a, nums b, nums c";
/// The drain cap plus the server noticing a closed socket.
const CLOSED_WITHIN: Duration = Duration::from_secs(25);

#[tokio::test]
async fn a_close_cut_whose_kill_cannot_connect_still_closes_the_connection() {
    for (label, fx) in my_fixtures("").await {
        fx.db.admin().await.query_drop(NUMS_SQL).await.unwrap();
        let fx = fx.proxied(Later::Refuse).await;
        let mut s = fx.session(SessionOptions::default()).await;
        let id = my_thread_id(&mut s).await;
        let mut limits = Limits::default();
        limits.max_rows = Some(10);
        limits.on_truncate = OnTruncate::Close;
        assert!(
            run(&mut s, HUGE, limits)
                .await
                .remove(0)
                .unwrap()
                .outcome
                .truncated,
            "{label}"
        );
        assert!(
            fx.quiet_within(&id, CLOSED_WITHIN).await,
            "{label}: the connection still reads"
        );
        let next = run(&mut s, "SELECT 1", Limits::default()).await.remove(0);
        assert!(matches!(next, Err(ExecuteError::SessionClosed)), "{label}");
    }
}

#[tokio::test]
async fn a_session_dropped_with_an_unread_result_closes_its_connection() {
    for (label, fx) in my_fixtures("").await {
        fx.db.admin().await.query_drop(NUMS_SQL).await.unwrap();
        let mut s = fx.session(SessionOptions::default()).await;
        let id = my_thread_id(&mut s).await;
        {
            let mut script = s.script(HUGE);
            let pending = script.next().await.unwrap().unwrap();
            let mut stream = pending.execute(Limits::default()).await.unwrap();
            assert!(stream.next_batch().await.unwrap().is_some(), "{label}");
        }
        drop(s);
        assert!(
            fx.quiet_within(&id, CLOSED_WITHIN).await,
            "{label}: the connection still reads"
        );
    }
}

#[tokio::test]
async fn settles_cut_short_share_one_drain_cap() {
    for (label, fx) in my_fixtures("").await {
        fx.db.admin().await.query_drop(NUMS_SQL).await.unwrap();
        let mut s = fx.session(SessionOptions::default()).await;
        // 999 padded rows stream out, then the last row sleeps 40 s on the server.
        let sql = "SELECT n, REPEAT('x', 100) AS pad, IF(n = 999, SLEEP(40), 0) AS slow \
                   FROM nums ORDER BY n";
        {
            let mut script = s.script(sql);
            let pending = script.next().await.unwrap().unwrap();
            let mut stream = pending.execute(Limits::default()).await.unwrap();
            while let Ok(batch) =
                tokio::time::timeout(Duration::from_millis(200), stream.next_batch()).await
            {
                assert!(
                    batch.unwrap().is_some(),
                    "{label}: ended before the slow row"
                );
            }
        }
        // Each settle is cut at 3 s; together they get one 10 s cap, not 10 s each.
        let started = Instant::now();
        let settled = loop {
            if let Ok(r) = tokio::time::timeout(Duration::from_secs(3), s.settle()).await {
                break r;
            }
            assert!(
                started.elapsed() < Duration::from_secs(20),
                "{label}: no cap across settles"
            );
        };
        assert!(
            matches!(settled, Err(ExecuteError::StoppedAtDrainCap)),
            "{label}: {settled:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(15),
            "{label}: {:?}",
            started.elapsed()
        );
    }
}
