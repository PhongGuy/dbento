//! A SQLite result parked at its row cap holds its statement on the worker until extended or dropped.
use std::time::{Duration, Instant};

use crate::support::{MANY, run, sqlite_fixture, text};
use dbento_core::{
    CellRef, ExtendError, Limits, OnTruncate, ResultStream, SessionOptions, TruncatedBy,
};

const ENDLESS: &str =
    "WITH RECURSIVE r(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM r) SELECT x FROM r";
/// 100 rows of exactly 100 bytes each.
const HUNDREDS: &str = "WITH RECURSIVE r(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM r WHERE x < 100) SELECT printf('%0100d', x) FROM r";

fn upto(n: u32) -> String {
    format!(
        "WITH RECURSIVE r(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM r WHERE x < {n}) SELECT x FROM r"
    )
}

fn park(rows: u64) -> Limits {
    let mut l = Limits::default();
    l.max_rows = Some(rows);
    l.on_truncate = OnTruncate::Park;
    l
}

async fn values(stream: &mut ResultStream<'_>) -> Vec<i64> {
    let mut out = Vec::new();
    while let Some(b) = stream.next_batch().await.unwrap() {
        for r in 0..b.len() {
            match b.cell(r, 0) {
                CellRef::Text(t) => out.push(t.parse().unwrap()),
                other @ (CellRef::Null | CellRef::Bytes(_) | _) => {
                    panic!("unexpected cell {other:?}")
                }
            }
        }
    }
    out
}

#[tokio::test]
async fn park_then_extend_delivers_the_rest_in_order() {
    let fx = sqlite_fixture("", "");
    let mut s = fx.session(SessionOptions::default()).await;
    let mut script = s.script(MANY);
    let p = script.next().await.unwrap().unwrap();
    let mut stream = p.execute(park(1000)).await.unwrap();
    assert_eq!(values(&mut stream).await, (1..=1000).collect::<Vec<i64>>());
    let o = stream.outcome().cloned().unwrap();
    assert_eq!(
        (o.rows, o.truncated, o.truncated_by),
        (1000, true, Some(TruncatedBy::Rows))
    );
    stream.extend(Some(1000)).await.unwrap();
    assert!(stream.outcome().is_none(), "extend clears the outcome");
    assert_eq!(
        values(&mut stream).await,
        (1001..=2000).collect::<Vec<i64>>()
    );
    assert_eq!(stream.outcome().unwrap().rows, 2000);
    stream.extend(None).await.unwrap();
    assert_eq!(
        values(&mut stream).await,
        (2001..=5000).collect::<Vec<i64>>()
    );
    let o = stream.outcome().cloned().unwrap();
    assert_eq!((o.rows, o.truncated, o.truncated_by), (5000, false, None));
    assert!(matches!(
        stream.extend(None).await,
        Err(ExtendError::NotParked)
    ));
}

// SQLite closes batches at 1,024 rows, so a cap of 1,024 falls on a batch boundary.
#[tokio::test]
async fn a_cap_on_a_batch_boundary_reads_one_row_past_it() {
    let fx = sqlite_fixture("", "");
    let mut s = fx.session(SessionOptions::default()).await;
    {
        let mut script = s.script(&upto(1024));
        let p = script.next().await.unwrap().unwrap();
        let mut stream = p.execute(park(1024)).await.unwrap();
        assert_eq!(values(&mut stream).await.len(), 1024);
        let o = stream.outcome().cloned().unwrap();
        assert_eq!((o.rows, o.truncated), (1024, false));
        assert!(matches!(
            stream.extend(None).await,
            Err(ExtendError::NotParked)
        ));
    }
    {
        let mut script = s.script(&upto(2048));
        let p = script.next().await.unwrap().unwrap();
        let mut stream = p.execute(park(1024)).await.unwrap();
        assert_eq!(values(&mut stream).await, (1..=1024).collect::<Vec<i64>>());
        assert!(stream.outcome().unwrap().truncated);
        stream.extend(None).await.unwrap();
        assert_eq!(
            values(&mut stream).await,
            (1025..=2048).collect::<Vec<i64>>()
        );
        assert!(!stream.outcome().unwrap().truncated);
    }
}

#[tokio::test]
async fn dropping_a_parked_stream_frees_the_worker_at_once() {
    let fx = sqlite_fixture("", "");
    let mut s = fx.session(SessionOptions::default()).await;
    {
        let mut script = s.script(ENDLESS);
        let p = script.next().await.unwrap().unwrap();
        let mut stream = p.execute(park(10)).await.unwrap();
        assert_eq!(values(&mut stream).await, (1..=10).collect::<Vec<i64>>());
    }
    let started = Instant::now();
    tokio::time::timeout(Duration::from_secs(2), s.settle())
        .await
        .expect("settle waited on a parked worker")
        .unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
    let r = run(&mut s, "SELECT 1", Limits::default()).await;
    assert_eq!(r[0].as_ref().unwrap().rows, vec![vec![text("1")]]);
}

#[tokio::test]
async fn a_byte_limit_still_applies_after_extend() {
    let fx = sqlite_fixture("", "");
    let mut s = fx.session(SessionOptions::default()).await;
    let mut limits = park(5);
    limits.max_bytes = Some(1000);
    {
        let mut script = s.script(HUNDREDS);
        let p = script.next().await.unwrap().unwrap();
        let mut stream = p.execute(limits).await.unwrap();
        assert_eq!(values(&mut stream).await, (1..=5).collect::<Vec<i64>>());
        stream.extend(None).await.unwrap();
        assert_eq!(values(&mut stream).await, (6..=10).collect::<Vec<i64>>());
        let o = stream.outcome().cloned().unwrap();
        assert_eq!((o.rows, o.truncated_by), (10, Some(TruncatedBy::Bytes)));
        assert!(matches!(
            stream.extend(None).await,
            Err(ExtendError::NotParked)
        ));
    }
    let r = run(&mut s, "SELECT 1", Limits::default()).await;
    assert_eq!(r[0].as_ref().unwrap().rows, vec![vec![text("1")]]);
}

#[tokio::test]
async fn a_cut_under_drain_cannot_extend() {
    let fx = sqlite_fixture("", "");
    let mut s = fx.session(SessionOptions::default()).await;
    let mut limits = Limits::default();
    limits.max_rows = Some(10);
    let mut script = s.script(MANY);
    let p = script.next().await.unwrap().unwrap();
    let mut stream = p.execute(limits).await.unwrap();
    assert_eq!(values(&mut stream).await.len(), 10);
    assert!(stream.outcome().unwrap().truncated);
    assert!(matches!(
        stream.extend(None).await,
        Err(ExtendError::NotParked)
    ));
}

#[tokio::test]
async fn the_extend_future_is_send() {
    fn is_send<T: Send>(_: &T) {}
    let fx = sqlite_fixture("", "");
    let mut s = fx.session(SessionOptions::default()).await;
    let mut script = s.script(MANY);
    let p = script.next().await.unwrap().unwrap();
    let mut stream = p.execute(park(10)).await.unwrap();
    is_send(&stream.extend(None));
}
