//! Row and byte cuts on SQL Server: Drain, Close with Attention, Park and the drain cap.

use std::time::{Duration, Instant};

use crate::support::{MsFixture, collect, ms_off, ms_spid, ms_this_session, ms_until, run, text};
use dbento_core::{
    ExecuteError, ExtendError, Limits, OnTruncate, ResultStream, SessionOptions, TruncatedBy,
};

const NUMS: &str = "SELECT TOP (1000) CAST(ROW_NUMBER() OVER (ORDER BY (SELECT NULL)) - 1 AS int) AS n \
                    INTO dbo.nums FROM sys.all_objects a CROSS JOIN sys.all_objects b";

async fn with_nums() -> (MsFixture, dbento_core::Session) {
    let fx = ms_off().await;
    let mut s = fx.session(SessionOptions::default()).await;
    run(&mut s, NUMS, Limits::default())
        .await
        .remove(0)
        .unwrap();
    (fx, s)
}

fn limits(rows: Option<u64>, bytes: Option<u64>, on: OnTruncate) -> Limits {
    let mut l = Limits::default();
    l.max_rows = rows;
    l.max_bytes = bytes;
    l.on_truncate = on;
    l
}

/// The first column of every row the stream delivers until its next outcome.
async fn texts(stream: &mut ResultStream<'_>) -> Vec<String> {
    let mut out = Vec::new();
    while let Some(b) = stream.next_batch().await.unwrap() {
        for r in 0..b.len() {
            match b.cell(r, 0) {
                dbento_core::CellRef::Text(t) => out.push(t.to_owned()),
                other => panic!("{other:?}"),
            }
        }
    }
    out
}

#[tokio::test]
async fn a_drained_cut_leaves_the_batch_running_its_later_items() {
    dbento_testkit::skip_without_mssql!();
    let (_fx, mut s) = with_nums().await;
    let ran = run(
        &mut s,
        "SELECT a.n FROM dbo.nums a CROSS JOIN dbo.nums b; SELECT 7 AS s",
        limits(Some(5), None, OnTruncate::Drain),
    )
    .await;
    let first = ran[0].as_ref().unwrap();
    assert_eq!((first.rows.len(), first.outcome.truncated), (5, true));
    assert_eq!(first.outcome.truncated_by, Some(TruncatedBy::Rows));
    assert_eq!(ran[1].as_ref().unwrap().rows, vec![vec![text("7")]]);
}

#[tokio::test]
async fn close_ends_the_session_at_the_cut() {
    dbento_testkit::skip_without_mssql!();
    let (_fx, mut s) = with_nums().await;
    let cut = run(
        &mut s,
        "SELECT n FROM dbo.nums",
        limits(Some(3), None, OnTruncate::Close),
    )
    .await;
    assert_eq!(cut[0].as_ref().unwrap().rows.len(), 3);
    assert!(cut[0].as_ref().unwrap().outcome.truncated);
    match run(&mut s, "SELECT 1 AS one", Limits::default())
        .await
        .remove(0)
    {
        Err(ExecuteError::SessionClosed) => {}
        other => panic!("{:?}", other.map(|r| r.rows)),
    }
}

#[tokio::test]
async fn park_holds_the_result_until_extend_then_reads_on() {
    dbento_testkit::skip_without_mssql!();
    let (_fx, mut s) = with_nums().await;
    {
        let mut sc = s.script("SELECT n FROM dbo.nums ORDER BY n");
        let p = sc.next().await.unwrap().unwrap();
        let mut stream = p
            .execute(limits(Some(2), None, OnTruncate::Park))
            .await
            .unwrap();
        assert_eq!(texts(&mut stream).await, ["0", "1"]);
        assert_eq!(
            stream.outcome().unwrap().truncated_by,
            Some(TruncatedBy::Rows)
        );
        stream.extend(Some(3)).await.unwrap();
        assert_eq!(texts(&mut stream).await, ["2", "3", "4"]);
        assert_eq!(
            (
                stream.outcome().unwrap().rows,
                stream.outcome().unwrap().truncated
            ),
            (5, true)
        );
        stream.extend(None).await.unwrap();
        assert_eq!(texts(&mut stream).await.len(), 995);
        let done = stream.outcome().unwrap();
        assert_eq!(
            (done.rows, done.truncated, done.extra_results),
            (1000, false, 0)
        );
    }
    assert_eq!(
        run(&mut s, "SELECT 1 AS one", Limits::default()).await[0]
            .as_ref()
            .unwrap()
            .rows,
        vec![vec![text("1")]]
    );
}

#[tokio::test]
async fn a_parked_last_query_counts_a_trailing_profile_set() {
    dbento_testkit::skip_without_mssql!();
    let (_fx, mut s) = with_nums().await;
    {
        let mut sc = s.script("SET STATISTICS PROFILE ON\nGO\nSELECT n FROM dbo.nums ORDER BY n");
        collect(sc.next().await.unwrap().unwrap(), Limits::default())
            .await
            .unwrap();
        let mut p = sc.next().await.unwrap().unwrap();
        assert!(p.is_last().await);
        let mut stream = p
            .execute(limits(Some(2), None, OnTruncate::Park))
            .await
            .unwrap();
        assert_eq!(texts(&mut stream).await, ["0", "1"]);
        stream.extend(None).await.unwrap();
        assert_eq!(texts(&mut stream).await.len(), 998);
        assert_eq!(
            stream.outcome().unwrap().extra_results,
            1,
            "the profile set is drained and counted"
        );
    }
    assert!(s.script("SELECT 1 AS one").next().await.unwrap().is_ok());
}

#[tokio::test]
async fn a_byte_cut_drains_even_under_park() {
    dbento_testkit::skip_without_mssql!();
    let (_fx, mut s) = with_nums().await;
    {
        let mut sc = s.script("SELECT REPLICATE(N'x', 100) AS s FROM dbo.nums");
        let p = sc.next().await.unwrap().unwrap();
        let mut stream = p
            .execute(limits(None, Some(150), OnTruncate::Park))
            .await
            .unwrap();
        assert_eq!(texts(&mut stream).await.len(), 2);
        assert_eq!(
            stream.outcome().unwrap().truncated_by,
            Some(TruncatedBy::Bytes)
        );
        assert!(matches!(
            stream.extend(None).await,
            Err(ExtendError::NotParked)
        ));
    }
    assert_eq!(
        run(&mut s, "SELECT 1 AS one", Limits::default()).await[0]
            .as_ref()
            .unwrap()
            .rows,
        vec![vec![text("1")]]
    );
}

#[tokio::test]
async fn a_dropped_stream_is_drained_and_the_next_query_is_correct() {
    dbento_testkit::skip_without_mssql!();
    let (_fx, mut s) = with_nums().await;
    {
        let mut sc = s.script("SELECT a.n FROM dbo.nums a CROSS JOIN dbo.nums b");
        let p = sc.next().await.unwrap().unwrap();
        let mut stream = p.execute(Limits::default()).await.unwrap();
        assert!(stream.next_batch().await.unwrap().is_some());
    }
    s.settle().await.unwrap();
    let ran = run(
        &mut s,
        "SELECT 1 AS one; SELECT 2 AS two",
        Limits::default(),
    )
    .await;
    assert_eq!(ran[0].as_ref().unwrap().rows, vec![vec![text("1")]]);
    assert_eq!(ran[1].as_ref().unwrap().rows, vec![vec![text("2")]]);
}

#[tokio::test]
async fn a_drain_past_the_cap_closes_the_session_once() {
    dbento_testkit::skip_without_mssql!();
    let (fx, mut s) = with_nums().await;
    let gone = ms_this_session(&mut s).await;
    let billion = "SELECT a.n FROM dbo.nums a CROSS JOIN dbo.nums b CROSS JOIN dbo.nums c";
    {
        let mut sc = s.script(billion);
        let p = sc.next().await.unwrap().unwrap();
        let cut = collect(p, limits(Some(1), None, OnTruncate::Drain))
            .await
            .unwrap();
        assert!(cut.outcome.truncated);
    }
    let started = Instant::now();
    assert!(matches!(
        s.settle().await,
        Err(ExecuteError::StoppedAtDrainCap)
    ));
    let waited = started.elapsed();
    assert!(
        waited >= Duration::from_secs(8) && waited < Duration::from_secs(30),
        "{waited:?}"
    );
    match run(&mut s, "SELECT 1 AS one", Limits::default())
        .await
        .remove(0)
    {
        Err(ExecuteError::SessionClosed) => {}
        other => panic!("{:?}", other.map(|r| r.rows)),
    }
    assert!(ms_until(&fx.db, &gone, "0").await);
}

#[tokio::test]
async fn close_stops_the_batch_with_attention_and_ends_the_session() {
    dbento_testkit::skip_without_mssql!();
    let (fx, mut s) = with_nums().await;
    let gone = ms_this_session(&mut s).await;
    let started = Instant::now();
    let cut = run(
        &mut s,
        "SELECT a.n FROM dbo.nums a CROSS JOIN dbo.nums b CROSS JOIN dbo.nums c",
        limits(Some(3), None, OnTruncate::Close),
    )
    .await;
    assert!(cut[0].as_ref().unwrap().outcome.truncated);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
    match run(&mut s, "SELECT 1 AS one", Limits::default())
        .await
        .remove(0)
    {
        Err(ExecuteError::SessionClosed) => {}
        other => panic!("{:?}", other.map(|r| r.rows)),
    }
    assert!(ms_until(&fx.db, &gone, "0").await);
}

#[tokio::test]
async fn dropping_a_session_mid_batch_stops_it_on_the_server() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_off().await;
    let mut s = fx.session(SessionOptions::default()).await;
    let spid = ms_spid(&mut s).await;
    let busy = format!("SELECT COUNT(*) FROM sys.dm_exec_requests WHERE session_id = {spid}");
    let gone = ms_this_session(&mut s).await;
    let work = tokio::spawn(async move {
        run(&mut s, "WAITFOR DELAY '00:01:00'", Limits::default())
            .await
            .len()
    });
    assert!(ms_until(&fx.db, &busy, "1").await, "the wait never started");
    work.abort();
    assert!(
        ms_until(&fx.db, &gone, "0").await,
        "the session outlived its drop"
    );
}
