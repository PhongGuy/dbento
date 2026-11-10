//! A Postgres result parked at its row cap resumes on `extend` without losing or repeating a row.
use crate::support::{collect, pg_fixture, run, text};
use dbento_core::{
    CellRef, ExtendError, Limits, OnTruncate, ResultStream, SessionOptions, TruncatedBy,
};

fn park(rows: u64) -> Limits {
    let mut l = Limits::default();
    l.max_rows = Some(rows);
    l.on_truncate = OnTruncate::Park;
    l
}

/// The first column of every row up to the stream's next `None`, as numbers.
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
    let fx = pg_fixture("").await;
    let mut s = fx.session(SessionOptions::default()).await;
    let mut script = s.script("SELECT g FROM generate_series(1, 2500) g");
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
    let o = stream.outcome().cloned().unwrap();
    assert_eq!((o.rows, o.truncated_by), (2000, Some(TruncatedBy::Rows)));
    stream.extend(None).await.unwrap();
    assert_eq!(
        values(&mut stream).await,
        (2001..=2500).collect::<Vec<i64>>()
    );
    let o = stream.outcome().cloned().unwrap();
    assert_eq!((o.rows, o.truncated, o.truncated_by), (2500, false, None));
    assert!(matches!(
        stream.extend(None).await,
        Err(ExtendError::NotParked)
    ));
}

#[tokio::test]
async fn exactly_max_rows_ends_complete_and_cannot_extend() {
    let fx = pg_fixture("").await;
    let mut s = fx.session(SessionOptions::default()).await;
    let mut script = s.script("SELECT g FROM generate_series(1, 1000) g");
    let p = script.next().await.unwrap().unwrap();
    let mut stream = p.execute(park(1000)).await.unwrap();
    assert_eq!(values(&mut stream).await.len(), 1000);
    let o = stream.outcome().cloned().unwrap();
    assert_eq!((o.rows, o.truncated, o.truncated_by), (1000, false, None));
    assert!(matches!(
        stream.extend(Some(10)).await,
        Err(ExtendError::NotParked)
    ));
}

#[tokio::test]
async fn a_cut_under_drain_cannot_extend() {
    let fx = pg_fixture("").await;
    let mut s = fx.session(SessionOptions::default()).await;
    let mut limits = Limits::default();
    limits.max_rows = Some(10);
    let mut script = s.script("SELECT g FROM generate_series(1, 5000) g");
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
async fn a_byte_limit_still_applies_after_extend() {
    let fx = pg_fixture("").await;
    let mut s = fx.session(SessionOptions::default()).await;
    let mut limits = park(5);
    limits.max_bytes = Some(1000);
    {
        let mut script =
            s.script("SELECT lpad(x::text, 100, '0') FROM generate_series(1, 100) AS x");
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
async fn a_dropped_parked_stream_leaves_the_session_usable() {
    let fx = pg_fixture("").await;
    let mut s = fx.session(SessionOptions::default()).await;
    let mut script = s.script("SELECT g FROM generate_series(1, 100000) g; SELECT 'after'");
    let p = script.next().await.unwrap().unwrap();
    let mut stream = p.execute(park(10)).await.unwrap();
    assert_eq!(values(&mut stream).await.len(), 10);
    drop(stream);
    let next = script.next().await.unwrap().unwrap();
    assert_eq!(
        collect(next, Limits::default()).await.unwrap().rows,
        vec![vec![text("after")]]
    );
}

#[tokio::test]
async fn a_parked_read_only_stream_settles_with_no_open_transaction() {
    let fx = pg_fixture("read_only = true").await;
    let mut s = fx.session(SessionOptions::default()).await;
    {
        let mut script = s.script("SELECT g FROM generate_series(1, 100000) g");
        let p = script.next().await.unwrap().unwrap();
        let mut stream = p.execute(park(10)).await.unwrap();
        assert_eq!(values(&mut stream).await.len(), 10);
        assert_eq!(
            stream.outcome().unwrap().truncated_by,
            Some(TruncatedBy::Rows)
        );
    }
    s.settle().await.unwrap();
    let open = "SELECT count(*) FROM pg_stat_activity WHERE application_name = 'dbento' AND datname = current_database() AND state LIKE 'idle in transaction%'";
    assert_eq!(fx.db.scalar(open).await.as_deref(), Some("0"));
    let r = run(&mut s, "SELECT 1", Limits::default()).await;
    assert_eq!(r[0].as_ref().unwrap().rows, vec![vec![text("1")]]);
}
