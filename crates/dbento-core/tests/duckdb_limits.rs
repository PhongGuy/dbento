#![cfg(feature = "duckdb")]
use crate::support::{duckdb_fixture, run, text};
use dbento_core::{
    ExecuteError, ExtendError, Limits, OnTruncate, ResultStream, SessionOptions, TruncatedBy,
};

fn rows(n: u64, on: OnTruncate) -> Limits {
    let mut l = Limits::default();
    l.max_rows = Some(n);
    l.on_truncate = on;
    l
}

async fn values(stream: &mut ResultStream<'_>) -> Vec<String> {
    let mut out = Vec::new();
    while let Some(b) = stream.next_batch().await.unwrap() {
        for r in 0..b.len() {
            match b.cell(r, 0) {
                dbento_core::CellRef::Text(t) => out.push(t.to_owned()),
                other @ (dbento_core::CellRef::Null | dbento_core::CellRef::Bytes(_) | _) => {
                    panic!("{other:?}")
                }
            }
        }
    }
    out
}

#[tokio::test]
async fn a_row_cut_under_drain_stops_a_read_and_keeps_the_session() {
    let fx = duckdb_fixture("", "");
    let mut s = fx.session(SessionOptions::default()).await;
    let r = run(
        &mut s,
        "SELECT * FROM range(100000)",
        rows(3, OnTruncate::Drain),
    )
    .await
    .remove(0)
    .unwrap();
    assert_eq!(r.rows.len(), 3);
    assert!(r.outcome.truncated);
    assert_eq!(r.outcome.truncated_by, Some(TruncatedBy::Rows));
    assert_eq!(
        run(&mut s, "SELECT 42", Limits::default())
            .await
            .remove(0)
            .unwrap()
            .rows,
        [vec![text("42")]]
    );
}

#[tokio::test]
async fn a_byte_cut_counts_cell_bytes() {
    let fx = duckdb_fixture("", "");
    let mut s = fx.session(SessionOptions::default()).await;
    let mut l = Limits::default();
    l.max_bytes = Some(10);
    let r = run(&mut s, "SELECT 'abcdef' FROM range(5)", l)
        .await
        .remove(0)
        .unwrap();
    assert_eq!(r.rows.len(), 2);
    assert_eq!(r.outcome.truncated_by, Some(TruncatedBy::Bytes));
}

#[tokio::test]
async fn close_ends_the_session_and_frees_the_file() {
    let fx = duckdb_fixture("", "");
    let mut s = fx.session(SessionOptions::default()).await;
    let r = run(
        &mut s,
        "SELECT * FROM range(100000)",
        rows(3, OnTruncate::Close),
    )
    .await
    .remove(0)
    .unwrap();
    assert!(r.outcome.truncated);
    match run(&mut s, "SELECT 1", Limits::default()).await.remove(0) {
        Err(ExecuteError::SessionClosed) => {}
        other => panic!("{:?}", other.err()),
    }
    let mut ro = SessionOptions::default();
    ro.force_read_only = true;
    fx.session(ro).await.close().await.unwrap();
}

#[tokio::test]
async fn park_then_extend_delivers_the_rest_in_order() {
    let fx = duckdb_fixture("", "");
    let mut s = fx.session(SessionOptions::default()).await;
    let mut script = s.script("SELECT CAST(range AS VARCHAR) FROM range(12)");
    let p = script.next().await.unwrap().unwrap();
    let mut stream = p.execute(rows(5, OnTruncate::Park)).await.unwrap();
    let mut seen = values(&mut stream).await;
    assert_eq!(seen.len(), 5);
    assert!(stream.outcome().unwrap().truncated);
    stream.extend(Some(4)).await.unwrap();
    seen.extend(values(&mut stream).await);
    assert_eq!(seen.len(), 9);
    stream.extend(None).await.unwrap();
    seen.extend(values(&mut stream).await);
    assert!(!stream.outcome().unwrap().truncated);
    assert_eq!(seen, (0..12).map(|i| i.to_string()).collect::<Vec<_>>());
}

#[tokio::test]
async fn a_dropped_parked_stream_frees_the_session() {
    let fx = duckdb_fixture("", "");
    let mut s = fx.session(SessionOptions::default()).await;
    {
        let mut script = s.script("SELECT * FROM range(100000)");
        let p = script.next().await.unwrap().unwrap();
        let mut stream = p.execute(rows(5, OnTruncate::Park)).await.unwrap();
        values(&mut stream).await;
    }
    assert_eq!(
        run(&mut s, "SELECT 42", Limits::default())
            .await
            .remove(0)
            .unwrap()
            .rows,
        [vec![text("42")]]
    );
}

// DuckDB applies a RETURNING write at execute, so a cut stream cannot undo it.
#[tokio::test]
async fn a_cut_write_was_committed_at_execute() {
    let fx = duckdb_fixture("CREATE TABLE t(a BIGINT)", "");
    let mut s = fx.session(SessionOptions::default()).await;
    let sql = "INSERT INTO t SELECT range FROM range(10000) RETURNING a";
    let r = run(&mut s, sql, rows(3, OnTruncate::Drain))
        .await
        .remove(0)
        .unwrap();
    assert_eq!(r.rows.len(), 3);
    assert!(r.outcome.truncated);
    s.close().await.unwrap();
    assert_eq!(fx.scalar("SELECT count(*) FROM t").await, text("10000"));
}

// DuckDB applies a RETURNING write at execute, so a cut stream cannot undo it.
#[tokio::test]
async fn a_dropped_write_stream_was_committed_at_execute() {
    let fx = duckdb_fixture("CREATE TABLE t(a BIGINT)", "");
    let mut s = fx.session(SessionOptions::default()).await;
    {
        let mut script = s.script("INSERT INTO t SELECT range FROM range(10000) RETURNING a");
        let p = script.next().await.unwrap().unwrap();
        let mut stream = p.execute(Limits::default()).await.unwrap();
        assert!(stream.next_batch().await.unwrap().is_some());
    }
    s.settle().await.unwrap();
    s.close().await.unwrap();
    assert_eq!(fx.scalar("SELECT count(*) FROM t").await, text("10000"));
}

#[tokio::test]
async fn a_cut_under_drain_cannot_extend() {
    let fx = duckdb_fixture("", "");
    let mut s = fx.session(SessionOptions::default()).await;
    let mut script = s.script("SELECT * FROM range(100)");
    let p = script.next().await.unwrap().unwrap();
    let mut stream = p.execute(rows(10, OnTruncate::Drain)).await.unwrap();
    while stream.next_batch().await.unwrap().is_some() {}
    assert!(matches!(
        stream.extend(None).await,
        Err(ExtendError::NotParked)
    ));
}
