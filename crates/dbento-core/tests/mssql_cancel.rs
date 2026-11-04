//! Cancel on SQL Server: Attention on the session's own connection stops the batch it was
//! aimed at, and the session goes on reading its own results.
mod support;
use std::time::{Duration, Instant};

use dbento_core::{CancelHandle, ExecuteError, Limits, SessionOptions};
use support::{collect, ms_off, ms_spid, ms_until, run, text};

const CUBE: &str = "SELECT a.object_id FROM sys.all_objects a CROSS JOIN sys.all_objects b \
                    CROSS JOIN sys.all_objects c";

fn cancel_after(cancel: CancelHandle, after: Duration) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        std::thread::sleep(after);
        cancel.cancel();
    })
}

#[tokio::test]
async fn a_cancel_stops_the_running_batch_and_the_session_reads_its_own_rows_after() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_off().await;
    let mut s = fx.session(SessionOptions::default()).await;
    for sql in [
        "WAITFOR DELAY '00:00:30'",
        "SELECT 1 AS early; WAITFOR DELAY '00:00:30'",
        CUBE,
    ] {
        for round in 0..2 {
            let t = cancel_after(s.cancel_handle(), Duration::from_millis(300));
            let started = Instant::now();
            let r =
                tokio::time::timeout(Duration::from_secs(20), run(&mut s, sql, Limits::default()))
                    .await
                    .expect("the cancel never arrived");
            t.join().unwrap();
            let last = r.last().and_then(|x| x.as_ref().err());
            assert!(
                matches!(last, Some(ExecuteError::Cancelled)),
                "{sql} {round}: {last:?}"
            );
            assert!(started.elapsed() < Duration::from_secs(5), "{sql} {round}");
            let after = run(&mut s, "SELECT 7 AS seven", Limits::default())
                .await
                .remove(0)
                .unwrap();
            assert_eq!(after.rows, vec![vec![text("7")]], "{sql} {round}");
        }
    }
}

#[tokio::test]
async fn attention_stops_the_rest_of_the_batch() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_off().await;
    let mut s = fx.session(SessionOptions::default()).await;
    run(&mut s, "CREATE TABLE dbo.c (n int)", Limits::default())
        .await
        .remove(0)
        .unwrap();
    let t = cancel_after(s.cancel_handle(), Duration::from_millis(300));
    let r = run(
        &mut s,
        "INSERT dbo.c VALUES (1); WAITFOR DELAY '00:00:10'; INSERT dbo.c VALUES (2)",
        Limits::default(),
    )
    .await;
    t.join().unwrap();
    // The server discards the first INSERT's buffered DONE on Attention, so its count never
    // arrives.
    assert_eq!(
        r.len(),
        1,
        "{:?}",
        r.iter().map(Result::is_ok).collect::<Vec<_>>()
    );
    assert!(matches!(r[0], Err(ExecuteError::Cancelled)));
    assert_eq!(
        fx.db.scalar("SELECT COUNT(*) FROM dbo.c").await.as_deref(),
        Some("1")
    );
}

#[tokio::test]
async fn a_cancel_between_prepare_and_execute_stops_the_batch_unsent() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_off().await;
    let mut s = fx.session(SessionOptions::default()).await;
    run(&mut s, "CREATE TABLE dbo.c (n int)", Limits::default())
        .await
        .remove(0)
        .unwrap();
    let cancel = s.cancel_handle();
    {
        let mut script = s.script("INSERT dbo.c VALUES (1)");
        let p = script.next().await.unwrap().unwrap();
        cancel.cancel();
        assert!(matches!(
            collect(p, Limits::default()).await,
            Err(ExecuteError::Cancelled)
        ));
    }
    s.settle().await.unwrap();
    assert_eq!(
        fx.db.scalar("SELECT COUNT(*) FROM dbo.c").await.as_deref(),
        Some("0")
    );
    // An idle cancel aims at nothing, so the next batch runs.
    cancel.cancel();
    let after = run(&mut s, "SELECT 7 AS seven", Limits::default())
        .await
        .remove(0)
        .unwrap();
    assert_eq!(after.rows, vec![vec![text("7")]]);
}

#[tokio::test]
async fn a_cancel_reaches_the_server_while_the_caller_reads_nothing() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_off().await;
    let mut s = fx.session(SessionOptions::default()).await;
    let spid = ms_spid(&mut s).await;
    let busy = format!("SELECT COUNT(*) FROM sys.dm_exec_requests WHERE session_id = {spid}");
    let cancel = s.cancel_handle();
    let mut script = s.script(CUBE);
    let p = script.next().await.unwrap().unwrap();
    let mut stream = p.execute(Limits::default()).await.unwrap();
    assert!(stream.next_batch().await.unwrap().is_some());
    assert!(ms_until(&fx.db, &busy, "1").await, "the batch never ran");
    cancel.cancel();
    assert!(
        ms_until(&fx.db, &busy, "0").await,
        "the batch runs on after the cancel"
    );
    let mut err = None;
    while err.is_none() {
        match stream.next_batch().await {
            Ok(Some(_)) => {}
            Ok(None) => break,
            Err(e) => err = Some(e),
        }
    }
    assert!(matches!(err, Some(ExecuteError::Cancelled)), "{err:?}");
}

// Attention rolls back under XACT_ABORT, and the stop can discard the ENVCHANGE that says so.
#[tokio::test]
async fn a_cancel_that_rolls_back_leaves_a_writable_session_usable() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_off().await;
    let mut s = fx.session(SessionOptions::default()).await;
    for sql in [
        "SET XACT_ABORT ON; BEGIN TRANSACTION; SELECT 1 AS a; WAITFOR DELAY '00:00:30'",
        &format!("SET XACT_ABORT ON; BEGIN TRANSACTION; {CUBE}"),
    ] {
        for round in 0..5 {
            let t = cancel_after(s.cancel_handle(), Duration::from_millis(300));
            let r = run(&mut s, sql, Limits::default()).await;
            t.join().unwrap();
            assert!(
                matches!(r.last(), Some(Err(ExecuteError::Cancelled))),
                "{sql} {round}"
            );
            let after = run(&mut s, "SELECT @@TRANCOUNT AS seen", Limits::default())
                .await
                .remove(0)
                .unwrap();
            assert_eq!(after.rows, vec![vec![text("0")]], "{sql} {round}");
        }
    }
}
