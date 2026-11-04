#![cfg(feature = "duckdb")]
//! Cancel and statement timeout on DuckDB sessions: an interrupt reaches only the statement
//! it was meant for, and who fired it decides between `Cancelled` and `Timeout`.
mod support;
use std::time::{Duration, Instant};

use dbento_core::{ExecuteError, Limits, OnTruncate, Session, SessionOptions};
use support::{duckdb_fixture, run, text};

/// Cannot finish within any test bound: 10^13 rows through a cross join.
const SPIN: &str = "SELECT count(*) FROM range(10000000) a, range(1000000) b";
/// `SPIN` as a write, which unlike a SELECT does its work inside execute.
const SPIN_WRITE: &str =
    "CREATE TABLE spun AS SELECT count(*) FROM range(10000000) a, range(1000000) b";

/// Fails a test that would otherwise hang on a statement nothing interrupts.
async fn bounded<T>(f: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(5), f)
        .await
        .expect("the statement was never interrupted")
}

fn with_timeout(ms: u64) -> SessionOptions {
    let mut o = SessionOptions::default();
    o.statement_timeout = Some(Duration::from_millis(ms));
    o
}

async fn select_one_works(s: &mut Session) {
    let r = run(s, "SELECT 1", Limits::default())
        .await
        .remove(0)
        .unwrap();
    assert_eq!(r.rows, vec![vec![text("1")]]);
}

#[tokio::test]
async fn cancel_from_another_task_interrupts_the_running_statement() {
    let fx = duckdb_fixture("", "");
    let mut s = fx.session(SessionOptions::default()).await;
    let handle = s.cancel_handle();
    let canceller = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(150)).await;
        handle.cancel();
    });
    let r = bounded(run(&mut s, SPIN, Limits::default())).await;
    assert!(
        matches!(r[0], Err(ExecuteError::Cancelled)),
        "{:?}",
        r[0].as_ref().err()
    );
    canceller.await.unwrap();
    select_one_works(&mut s).await;
    s.close().await.unwrap();
}

#[tokio::test]
async fn a_cancel_right_after_execute_is_not_lost() {
    let fx = duckdb_fixture("", "");
    let mut s = fx.session(SessionOptions::default()).await;
    let handle = s.cancel_handle();
    let mut script = s.script(SPIN);
    let pending = script.next().await.unwrap().unwrap();
    // join! polls execute first, and execute sends its request before its first await, so
    // the cancel lands after the statement was asked for and before it starts running.
    let (result, ()) = bounded(async {
        tokio::join!(pending.execute(Limits::default()), async {
            handle.cancel()
        })
    })
    .await;
    assert!(
        matches!(result, Err(ExecuteError::Cancelled)),
        "{:?}",
        result.err()
    );
    drop(script);
    select_one_works(&mut s).await;
    s.close().await.unwrap();
}

#[tokio::test]
async fn a_cancel_mid_stream_fails_the_stream_as_cancelled() {
    let fx = duckdb_fixture("", "");
    let mut s = fx.session(SessionOptions::default()).await;
    let handle = s.cancel_handle();
    {
        let mut script = s.script("SELECT * FROM range(100000000)");
        let pending = script.next().await.unwrap().unwrap();
        let mut stream = pending.execute(Limits::default()).await.unwrap();
        assert!(stream.next_batch().await.unwrap().is_some());
        handle.cancel();
        let end = bounded(async {
            loop {
                match stream.next_batch().await {
                    Ok(Some(_)) => continue,
                    other => break other,
                }
            }
        })
        .await;
        assert!(matches!(end, Err(ExecuteError::Cancelled)), "{end:?}");
    }
    select_one_works(&mut s).await;
    s.close().await.unwrap();
}

#[tokio::test]
async fn a_cancel_between_statements_does_not_hit_the_next_one() {
    let fx = duckdb_fixture("", "");
    let mut s = fx.session(SessionOptions::default()).await;
    select_one_works(&mut s).await;
    s.cancel_handle().cancel();
    select_one_works(&mut s).await;
    s.close().await.unwrap();
}

#[tokio::test]
async fn a_cancel_racing_short_statements_only_ever_cancels() {
    let fx = duckdb_fixture("", "");
    let mut s = fx.session(with_timeout(30_000)).await;
    let handle = s.cancel_handle();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let canceller = std::thread::spawn({
        let stop = stop.clone();
        move || {
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                handle.cancel();
                std::thread::sleep(Duration::from_micros(40));
            }
        }
    });
    // A statement either runs or reports its own cancel; a stray fire would show as anything else.
    let started = Instant::now();
    let mut ran = 0;
    while started.elapsed() < Duration::from_secs(2) {
        for r in run(&mut s, "SELECT 1; SELECT 2; SELECT 3", Limits::default()).await {
            match r {
                Ok(_) | Err(ExecuteError::Cancelled) => ran += 1,
                Err(e) => {
                    stop.store(true, std::sync::atomic::Ordering::Relaxed);
                    panic!("after {ran} statements, a racing cancel caused {e:?}");
                }
            }
        }
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    canceller.join().unwrap();
    s.close().await.unwrap();
}

#[tokio::test]
async fn a_statement_timeout_interrupts_a_long_query() {
    for (extra, opts) in [
        ("statement_timeout = \"200ms\"\n", SessionOptions::default()),
        ("", with_timeout(200)),
    ] {
        let fx = duckdb_fixture("", extra);
        let mut s = fx.session(opts).await;
        let started = Instant::now();
        let r = bounded(run(&mut s, SPIN, Limits::default())).await;
        let took = started.elapsed();
        assert!(
            matches!(r[0], Err(ExecuteError::Timeout)),
            "config {extra:?}: {:?}",
            r[0].as_ref().err()
        );
        assert!(
            took >= Duration::from_millis(150) && took < Duration::from_secs(3),
            "{took:?}"
        );
        select_one_works(&mut s).await;
        s.close().await.unwrap();
    }
}

#[tokio::test]
async fn a_deadline_that_passes_while_parked_fails_the_resumed_stream() {
    let fx = duckdb_fixture("", "");
    let mut s = fx.session(with_timeout(200)).await;
    {
        let mut script = s.script("SELECT * FROM range(100000000)");
        let pending = script.next().await.unwrap().unwrap();
        let mut park = Limits::default();
        park.max_rows = Some(1);
        park.on_truncate = OnTruncate::Park;
        let mut stream = pending.execute(park).await.unwrap();
        while stream.next_batch().await.unwrap().is_some() {}
        assert!(stream.outcome().unwrap().truncated);
        tokio::time::sleep(Duration::from_millis(400)).await;
        stream.extend(None).await.unwrap();
        let end = bounded(async {
            loop {
                match stream.next_batch().await {
                    Ok(Some(_)) => continue,
                    other => break other,
                }
            }
        })
        .await;
        assert!(matches!(end, Err(ExecuteError::Timeout)), "{end:?}");
    }
    select_one_works(&mut s).await;
    s.close().await.unwrap();
}

#[tokio::test]
async fn a_timeout_on_a_trusted_read_only_session_rolls_back_and_keeps_it_usable() {
    let fx = duckdb_fixture("CREATE TABLE t(a INTEGER)", "");
    let mut writer = fx.session(SessionOptions::default()).await;
    let mut o = with_timeout(200);
    o.force_read_only = true;
    o.trusted_sql = true;
    let mut ro = fx.session(o).await;
    let r = bounded(run(&mut ro, SPIN, Limits::default())).await;
    assert!(
        matches!(r[0], Err(ExecuteError::Timeout)),
        "{:?}",
        r[0].as_ref().err()
    );
    // The wrapping ROLLBACK ran, so the next statement begins its own transaction and sees
    // the writer's later row.
    select_one_works(&mut ro).await;
    run(&mut writer, "INSERT INTO t VALUES (1)", Limits::default())
        .await
        .remove(0)
        .unwrap();
    let seen = run(&mut ro, "SELECT count(*) FROM t", Limits::default())
        .await
        .remove(0)
        .unwrap();
    assert_eq!(seen.rows, vec![vec![text("1")]]);
    ro.close().await.unwrap();
    writer.close().await.unwrap();
}

#[tokio::test]
async fn a_row_cut_on_a_trusted_read_only_session_rolls_back_before_the_next_statement() {
    let fx = duckdb_fixture(
        "CREATE TABLE t(a INTEGER); INSERT INTO t VALUES (1), (2)",
        "",
    );
    let mut writer = fx.session(SessionOptions::default()).await;
    let mut o = with_timeout(30_000);
    o.force_read_only = true;
    o.trusted_sql = true;
    let mut ro = fx.session(o).await;
    let mut cut = Limits::default();
    cut.max_rows = Some(1);
    let r = run(&mut ro, "SELECT a FROM t ORDER BY a", cut)
        .await
        .remove(0)
        .unwrap();
    assert!(r.outcome.truncated);
    run(&mut writer, "INSERT INTO t VALUES (3)", Limits::default())
        .await
        .remove(0)
        .unwrap();
    // A transaction left open by the cut would still read its old snapshot of two rows.
    let seen = run(&mut ro, "SELECT count(*) FROM t", Limits::default())
        .await
        .remove(0)
        .unwrap();
    assert_eq!(seen.rows, vec![vec![text("3")]]);
    ro.close().await.unwrap();
    writer.close().await.unwrap();
}

#[tokio::test]
async fn close_interrupts_a_statement_whose_execute_was_dropped() {
    let fx = duckdb_fixture("", "");
    let mut s = fx.session(SessionOptions::default()).await;
    {
        let mut script = s.script(SPIN_WRITE);
        let pending = script.next().await.unwrap().unwrap();
        // The caller gives up while the worker is still computing the statement.
        let gave_up = tokio::time::timeout(
            Duration::from_millis(200),
            pending.execute(Limits::default()),
        )
        .await;
        assert!(gave_up.is_err(), "the statement cannot finish in 200 ms");
    }
    // close waits out DRAIN_CAP (10 s), then cancels; the worker lets go within a second.
    let started = Instant::now();
    tokio::time::timeout(Duration::from_secs(15), s.close())
        .await
        .expect("close returns")
        .unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(12),
        "{:?}",
        started.elapsed()
    );
    // The lease is gone, so the file opens in the other mode.
    let mut o = SessionOptions::default();
    o.force_read_only = true;
    fx.session(o).await.close().await.unwrap();
}
