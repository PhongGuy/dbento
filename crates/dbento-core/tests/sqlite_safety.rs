use crate::support::{LONG, MANY, run, sqlite_fixture, text};
use dbento_core::{ExecuteError, Limits, OnTruncate, SessionOptions};
use std::time::{Duration, Instant};

/// Fails a test that would otherwise hang on a query that is never interrupted.
async fn bounded<T>(f: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(5), f)
        .await
        .expect("the statement was never interrupted")
}

const T3: &str = "CREATE TABLE t(a INTEGER); INSERT INTO t VALUES (1),(2),(3)";

fn forced_read_only() -> SessionOptions {
    let mut o = SessionOptions::default();
    o.force_read_only = true;
    o
}

fn close_after(max_rows: Option<u64>) -> Limits {
    let mut l = Limits::default();
    l.max_rows = max_rows;
    l.on_truncate = OnTruncate::Close;
    l
}

#[tokio::test]
async fn a_read_only_config_rejects_insert() {
    let fx = sqlite_fixture(T3, "read_only = true\n");
    let mut s = fx.session(SessionOptions::default()).await;
    assert!(s.is_read_only());
    let r = run(&mut s, "INSERT INTO t VALUES (4)", Limits::default()).await;
    assert!(matches!(r[0], Err(ExecuteError::ReadOnlyViolation { .. })));
    assert_eq!(fx.count("SELECT count(*) FROM t"), 3);
}

#[tokio::test]
async fn force_read_only_blocks_the_query_only_and_attach_escapes() {
    let fx = sqlite_fixture(T3, "");
    let other = fx.dir.path().join("other.db");
    rusqlite::Connection::open(&other)
        .unwrap()
        .execute_batch("CREATE TABLE t(a INTEGER)")
        .unwrap();
    let mut s = fx.session(forced_read_only()).await;
    assert!(s.is_read_only());

    let r = run(
        &mut s,
        "PRAGMA query_only = 0; INSERT INTO t VALUES (4)",
        Limits::default(),
    )
    .await;
    assert!(r[0].is_ok(), "the pragma itself runs");
    assert!(matches!(r[1], Err(ExecuteError::ReadOnlyViolation { .. })));

    let r = run(
        &mut s,
        &format!(
            "ATTACH '{}' AS o; INSERT INTO o.t VALUES (1)",
            other.display()
        ),
        Limits::default(),
    )
    .await;
    assert_eq!(r.len(), 1);
    assert!(r[0].is_err(), "ATTACH must fail");

    let r = run(
        &mut s,
        &format!("ATTACH 'file:{}?mode=rw' AS o", other.display()),
        Limits::default(),
    )
    .await;
    assert!(r[0].is_err(), "a mode=rw URI must not widen access");

    assert_eq!(fx.count("SELECT count(*) FROM t"), 3);
    let o: i64 = rusqlite::Connection::open(&other)
        .unwrap()
        .query_row("SELECT count(*) FROM t", [], |r| r.get(0))
        .unwrap();
    assert_eq!(o, 0);
}

#[tokio::test]
async fn statement_timeout_interrupts_a_long_query() {
    for (extra, opts) in [
        ("statement_timeout = \"200ms\"\n", SessionOptions::default()),
        ("", {
            let mut o = SessionOptions::default();
            o.statement_timeout = Some(Duration::from_millis(200));
            o
        }),
    ] {
        let fx = sqlite_fixture("", extra);
        let mut s = fx.session(opts).await;
        let started = Instant::now();
        let r = bounded(run(&mut s, LONG, Limits::default())).await;
        let took = started.elapsed();
        assert!(
            matches!(r[0], Err(ExecuteError::Timeout)),
            "config {extra:?}"
        );
        assert!(
            took >= Duration::from_millis(150) && took < Duration::from_secs(3),
            "{took:?}"
        );
        assert_eq!(
            run(&mut s, "SELECT 1", Limits::default())
                .await
                .remove(0)
                .unwrap()
                .rows,
            vec![vec![text("1")]]
        );
    }
}

#[tokio::test]
async fn cancel_from_another_task_interrupts_the_running_statement() {
    let fx = sqlite_fixture("", "");
    let mut s = fx.session(SessionOptions::default()).await;
    let handle = s.cancel_handle();
    let canceller = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        handle.cancel();
    });
    let r = bounded(run(&mut s, LONG, Limits::default())).await;
    assert!(matches!(r[0], Err(ExecuteError::Cancelled)));
    canceller.await.unwrap();
}

#[tokio::test]
async fn a_cancel_right_after_execute_is_not_lost() {
    let fx = sqlite_fixture("", "");
    let mut s = fx.session(SessionOptions::default()).await;
    let handle = s.cancel_handle();
    let mut script = s.script(LONG);
    let pending = script.next().await.unwrap().unwrap();
    // join! polls execute first, and execute sends its Ctl before its first await,
    // so the Execute message is queued before the cancel runs.
    let (result, ()) = bounded(async {
        tokio::join!(pending.execute(Limits::default()), async {
            handle.cancel()
        })
    })
    .await;
    assert!(matches!(result, Err(ExecuteError::Cancelled)));
}

#[tokio::test]
async fn a_cancel_between_statements_does_not_hit_the_next_one() {
    let fx = sqlite_fixture("", "");
    let mut s = fx.session(SessionOptions::default()).await;
    s.cancel_handle().cancel();
    assert_eq!(
        run(&mut s, "SELECT 1", Limits::default())
            .await
            .remove(0)
            .unwrap()
            .rows,
        vec![vec![text("1")]]
    );
}

#[tokio::test]
async fn truncation_under_close_ends_the_session() {
    let fx = sqlite_fixture(T3, "");
    let mut s = fx.session(SessionOptions::default()).await;
    let r = run(&mut s, "SELECT a FROM t; SELECT 2", close_after(None)).await;
    assert!(r.iter().all(Result::is_ok), "no truncation, no close");
    let r = run(&mut s, "SELECT a FROM t; SELECT 2", close_after(Some(1))).await;
    assert_eq!(r.len(), 2);
    let Ok(first) = &r[0] else {
        panic!("the first statement must run")
    };
    assert!(first.outcome.truncated);
    assert!(matches!(r[1], Err(ExecuteError::SessionClosed)));
    assert!(matches!(
        run(&mut s, "SELECT 1", Limits::default()).await[0],
        Err(ExecuteError::SessionClosed)
    ));
}

#[tokio::test]
async fn a_stream_dropped_early_under_close_ends_the_session() {
    let fx = sqlite_fixture("", "");
    let mut s = fx.session(SessionOptions::default()).await;
    {
        let mut script = s.script(&format!("{MANY}; SELECT 2"));
        let pending = script.next().await.unwrap().unwrap();
        let mut stream = pending.execute(close_after(None)).await.unwrap();
        assert!(stream.next_batch().await.unwrap().is_some());
        drop(stream);
        assert!(matches!(
            script.next().await.unwrap(),
            Err(ExecuteError::SessionClosed)
        ));
    }
    assert!(matches!(
        run(&mut s, "SELECT 1", Limits::default()).await[0],
        Err(ExecuteError::SessionClosed)
    ));
}

#[tokio::test]
async fn a_cancel_racing_short_statements_only_ever_cancels() {
    let fx = sqlite_fixture("", "");
    let mut s = fx.session(SessionOptions::default()).await;
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
    // Without a timeout, a statement either runs or reports its own cancel; nothing else.
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
}
