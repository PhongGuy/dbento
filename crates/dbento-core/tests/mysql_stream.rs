//! MySQL streaming: batches leave as rows arrive, a dropped stream is settled, and the result
//! sets after the first are counted and drained.
use std::time::{Duration, Instant};

use crate::support::{Cell, my_fixtures, run, sqlite_fixture, text};
use dbento_core::{ExecuteError, Limits, SessionOptions};
use dbento_testkit::NUMS_SQL;
use mysql_async::prelude::*;

#[tokio::test]
async fn the_first_batch_leaves_before_a_slow_tail_arrives() {
    for (label, fx) in my_fixtures("").await {
        fx.db.admin().await.query_drop(NUMS_SQL).await.unwrap();
        let mut s = fx.session(SessionOptions::default()).await;
        // 999 rows of about 100 bytes, then one that sleeps 3 s on the server.
        let sql = "SELECT n, REPEAT('x', 100) AS pad, IF(n = 999, SLEEP(3), 0) AS slow FROM nums ORDER BY n";
        let mut script = s.script(sql);
        let mut stream = script
            .next()
            .await
            .unwrap()
            .unwrap()
            .execute(Limits::default())
            .await
            .unwrap();
        let started = Instant::now();
        let first = stream.next_batch().await.unwrap().unwrap();
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{label}: waited {:?}",
            started.elapsed()
        );
        assert!(
            !first.is_empty() && first.len() < 1000,
            "{label}: {} rows",
            first.len()
        );
        let mut total = first.len();
        while let Some(b) = stream.next_batch().await.unwrap() {
            total += b.len();
        }
        assert_eq!(total, 1000, "{label}");
    }
}

#[tokio::test]
async fn a_large_result_streams_in_order_across_batches() {
    for (label, fx) in my_fixtures("").await {
        fx.db.admin().await.query_drop(NUMS_SQL).await.unwrap();
        let mut s = fx.session(SessionOptions::default()).await;
        let sql = "SELECT a.n * 1000 + b.n AS x FROM nums a, nums b ORDER BY x LIMIT 100000";
        let ran = run(&mut s, sql, Limits::default()).await.remove(0).unwrap();
        let want: Vec<Vec<Cell>> = (0..100_000)
            .map(|n: i64| vec![text(&n.to_string())])
            .collect();
        assert!(ran.rows == want, "{label}: {} rows", ran.rows.len());
        assert_eq!(
            (ran.outcome.rows, ran.outcome.truncated),
            (100_000, false),
            "{label}"
        );
    }
}

#[tokio::test]
async fn a_stream_dropped_after_its_first_batch_leaves_the_session_usable() {
    for (label, fx) in my_fixtures("").await {
        fx.db.admin().await.query_drop(NUMS_SQL).await.unwrap();
        let mut s = fx.session(SessionOptions::default()).await;
        let mut script = s.script("SELECT a.n * 1000 + b.n AS x FROM nums a, nums b");
        let mut stream = script
            .next()
            .await
            .unwrap()
            .unwrap()
            .execute(Limits::default())
            .await
            .unwrap();
        assert!(
            !stream.next_batch().await.unwrap().unwrap().is_empty(),
            "{label}"
        );
        drop(stream);
        drop(script);
        let started = Instant::now();
        assert_eq!(
            run(&mut s, "SELECT 7", Limits::default())
                .await
                .remove(0)
                .unwrap()
                .rows,
            vec![vec![text("7")]]
        );
        assert!(started.elapsed() < Duration::from_secs(10), "{label}");
    }
}

#[tokio::test]
async fn a_call_shows_its_first_result_set_and_counts_the_rest() {
    for (label, fx) in my_fixtures("").await {
        let mut admin = fx.db.admin().await;
        for body in [
            "CREATE PROCEDURE two() BEGIN SELECT 1 AS a; SELECT 2 AS b, 3 AS c; END",
            "CREATE PROCEDURE quiet() BEGIN DECLARE x INT DEFAULT 1; SET x = x + 1; END",
            "CREATE PROCEDURE broken() BEGIN SELECT 1 AS a; SELECT * FROM missing_t; END",
        ] {
            admin.query_drop(body).await.unwrap();
        }
        let mut s = fx.session(SessionOptions::default()).await;
        let ran = run(&mut s, "CALL two()", Limits::default())
            .await
            .remove(0)
            .unwrap();
        assert_eq!(
            ran.columns
                .iter()
                .map(|(n, _)| n.as_str())
                .collect::<Vec<_>>(),
            ["a"],
            "{label}"
        );
        assert_eq!(ran.rows, vec![vec![text("1")]], "{label}");
        assert_eq!(
            (ran.outcome.rows, ran.outcome.extra_results),
            (1, 1),
            "{label}"
        );
        let ran = run(&mut s, "CALL quiet()", Limits::default())
            .await
            .remove(0)
            .unwrap();
        assert!(ran.columns.is_empty(), "{label}");
        assert_eq!(ran.outcome.extra_results, 0, "{label}");
        match run(&mut s, "CALL broken()", Limits::default())
            .await
            .remove(0)
        {
            Err(ExecuteError::Database { code, .. }) => assert_eq!(code, "1146", "{label}"),
            other => panic!("{label}: {:?}", other.err()),
        }
        assert_eq!(
            run(&mut s, "SELECT 7", Limits::default())
                .await
                .remove(0)
                .unwrap()
                .rows,
            vec![vec![text("7")]]
        );
    }
}

/// A server error partway through a first result set ends the response, so nothing is left for
/// mysql_async to drain uncapped on the next statement.
#[tokio::test]
async fn a_server_error_partway_through_a_result_leaves_the_session_usable() {
    for (label, fx) in my_fixtures("").await {
        let mut admin = fx.db.admin().await;
        admin.query_drop(NUMS_SQL).await.unwrap();
        // The correlated subquery returns many rows only from n = 900 on (error 1242), and the
        // set that would follow sleeps, so an uncapped drain of it would show in the timing.
        admin
            .query_drop(
                "CREATE PROCEDURE midfail() BEGIN \
                 SELECT t.n, (SELECT m.n FROM nums m WHERE t.n >= 900 OR m.n = t.n) AS s \
                 FROM nums t ORDER BY t.n; \
                 SELECT SLEEP(5) AS slow; END",
            )
            .await
            .unwrap();
        let mut s = fx.session(SessionOptions::default()).await;
        for sql in ["CALL midfail()"] {
            let started = Instant::now();
            match run(&mut s, sql, Limits::default()).await.pop().unwrap() {
                Err(ExecuteError::Database { code, .. }) => {
                    assert_eq!(code, "1242", "{label}: {sql}")
                }
                other => panic!("{label}: {sql}: {:?}", other.err()),
            }
            assert_eq!(
                run(&mut s, "SELECT 7", Limits::default())
                    .await
                    .remove(0)
                    .unwrap()
                    .rows,
                vec![vec![text("7")]],
                "{label}: {sql}"
            );
            assert!(
                started.elapsed() < Duration::from_secs(4),
                "{label}: {sql}: {:?}",
                started.elapsed()
            );
        }
    }
}

#[tokio::test]
async fn other_engines_report_no_extra_result_sets() {
    let fx = sqlite_fixture("", "");
    let mut s = fx.session(SessionOptions::default()).await;
    let ran = run(&mut s, "SELECT 1", Limits::default())
        .await
        .remove(0)
        .unwrap();
    assert_eq!(ran.outcome.extra_results, 0);
}

/// A next_batch dropped while its row read waits leaves the read for the next call, and a
/// stream dropped then leaves it for settle.
#[tokio::test]
async fn a_next_batch_dropped_mid_read_keeps_the_session() {
    for (label, fx) in my_fixtures("").await {
        fx.db.admin().await.query_drop(NUMS_SQL).await.unwrap();
        let mut s = fx.session(SessionOptions::default()).await;
        for drop_after_cancel in [false, true] {
            // 999 padded rows stream out, so the read of the sleeping last row is in flight.
            let sql = "SELECT n, REPEAT('x', 100) AS pad, IF(n = 999, SLEEP(1), 0) AS slow \
                       FROM nums ORDER BY n";
            let mut script = s.script(sql);
            let mut stream = script
                .next()
                .await
                .unwrap()
                .unwrap()
                .execute(Limits::default())
                .await
                .unwrap();
            let mut total = 0;
            while let Ok(batch) =
                tokio::time::timeout(Duration::from_millis(200), stream.next_batch()).await
            {
                total += batch.unwrap().expect("a batch before the slow row").len();
            }
            assert!(total < 1000, "{label}: {total} rows before the cut");
            if !drop_after_cancel {
                while let Some(b) = stream.next_batch().await.unwrap() {
                    total += b.len();
                }
                assert_eq!(total, 1000, "{label}");
                assert_eq!(stream.outcome().unwrap().rows, 1000, "{label}");
            }
            drop(stream);
            drop(script);
            assert_eq!(
                run(&mut s, "SELECT 7", Limits::default())
                    .await
                    .remove(0)
                    .unwrap()
                    .rows,
                vec![vec![text("7")]],
                "{label}: dropped after cancel {drop_after_cancel}"
            );
        }
    }
}

/// A next_batch dropped while the later result sets drain keeps the drain and its count.
#[tokio::test]
async fn a_next_batch_dropped_mid_drain_keeps_the_count() {
    for (label, fx) in my_fixtures("").await {
        fx.db
            .admin()
            .await
            .query_drop("CREATE PROCEDURE tail() BEGIN SELECT 1 AS a; SELECT SLEEP(1) AS b; END")
            .await
            .unwrap();
        let mut s = fx.session(SessionOptions::default()).await;
        let mut script = s.script("CALL tail()");
        let mut stream = script
            .next()
            .await
            .unwrap()
            .unwrap()
            .execute(Limits::default())
            .await
            .unwrap();
        assert_eq!(
            stream.next_batch().await.unwrap().unwrap().len(),
            1,
            "{label}"
        );
        let cut = tokio::time::timeout(Duration::from_millis(200), stream.next_batch()).await;
        assert!(cut.is_err(), "{label}: the drain finished early");
        assert!(stream.next_batch().await.unwrap().is_none(), "{label}");
        let outcome = stream.outcome().unwrap();
        assert_eq!((outcome.rows, outcome.extra_results), (1, 1), "{label}");
    }
}

/// A row read left in flight by a cancelled next_batch and a dropped stream fails on the
/// server; settle finishes the read and reports its error instead of discarding it.
#[tokio::test]
async fn settle_reports_the_error_of_a_read_left_in_flight() {
    for (label, fx) in my_fixtures("").await {
        fx.db.admin().await.query_drop(NUMS_SQL).await.unwrap();
        let mut s = fx.session(SessionOptions::default()).await;
        // 999 padded rows stream out, then the last row sleeps and fails (error 1242).
        let sql = "SELECT t.n, REPEAT('x', 100) AS pad, CASE WHEN t.n < 999 THEN 0 \
                   WHEN SLEEP(2) = 0 THEN (SELECT m.n FROM nums m WHERE t.n >= 999 OR m.n = t.n) \
                   END AS s FROM nums t ORDER BY t.n";
        let mut script = s.script(sql);
        let mut stream = script
            .next()
            .await
            .unwrap()
            .unwrap()
            .execute(Limits::default())
            .await
            .unwrap();
        while let Ok(batch) =
            tokio::time::timeout(Duration::from_millis(500), stream.next_batch()).await
        {
            assert!(
                batch.unwrap().is_some(),
                "{label}: ended before the slow row"
            );
        }
        drop(stream);
        drop(script);
        match s.settle().await {
            Err(ExecuteError::Database { code, .. }) => assert_eq!(code, "1242", "{label}"),
            other => panic!("{label}: {other:?}"),
        }
        assert_eq!(
            run(&mut s, "SELECT 7", Limits::default())
                .await
                .remove(0)
                .unwrap()
                .rows,
            vec![vec![text("7")]],
            "{label}"
        );
    }
}
