//! The byte bound: a result stops once the rows delivered hold the budget and another row exists.
mod support;
use dbento_core::{ExecuteError, Limits, OnTruncate, Session, SessionOptions, TruncatedBy};
use support::{pg_fixture, run, sqlite_fixture};

/// 100 rows of exactly 100 bytes each.
const LITE_HUNDREDS: &str = "WITH RECURSIVE r(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM r WHERE x < 100) SELECT printf('%0100d', x) FROM r";
const PG_HUNDREDS: &str = "SELECT lpad(x::text, 100, '0') FROM generate_series(1, 100) AS x";

fn limits(max_rows: Option<u64>, max_bytes: Option<u64>) -> Limits {
    let mut l = Limits::default();
    l.max_rows = max_rows;
    l.max_bytes = max_bytes;
    l
}

/// (rows collected, outcome rows, truncated, truncated_by) of the first statement.
async fn shape(s: &mut Session, sql: &str, l: Limits) -> (usize, u64, bool, Option<TruncatedBy>) {
    let ran = run(s, sql, l).await.remove(0).unwrap();
    let o = ran.outcome;
    (ran.rows.len(), o.rows, o.truncated, o.truncated_by)
}

async fn bounds_hold(s: &mut Session, hundreds: &str) {
    let bytes = Some(TruncatedBy::Bytes);
    assert_eq!(
        shape(s, hundreds, limits(None, Some(1000))).await,
        (10, 10, true, bytes)
    );
    // 1,000 bytes are under 1,050, so an eleventh row joins: the budget plus at most one row.
    assert_eq!(
        shape(s, hundreds, limits(None, Some(1050))).await,
        (11, 11, true, bytes)
    );
    assert_eq!(
        shape(s, hundreds, limits(Some(10), Some(1000))).await,
        (10, 10, true, Some(TruncatedBy::Rows)),
        "the row bound wins a tie"
    );
    assert_eq!(
        shape(s, hundreds, limits(Some(5), None)).await,
        (5, 5, true, Some(TruncatedBy::Rows))
    );
    // Reaching the budget on the last row truncates nothing.
    assert_eq!(
        shape(s, hundreds, limits(None, Some(10_000))).await,
        (100, 100, false, None)
    );
    assert_eq!(
        shape(s, hundreds, limits(None, None)).await,
        (100, 100, false, None)
    );
}

async fn close_ends_the_session(s: &mut Session, hundreds: &str) {
    let mut l = limits(None, Some(1000));
    l.on_truncate = OnTruncate::Close;
    let r = run(s, &format!("{hundreds}; SELECT 2"), l).await;
    assert_eq!(r.len(), 2);
    assert_eq!(
        r[0].as_ref().unwrap().outcome.truncated_by,
        Some(TruncatedBy::Bytes)
    );
    assert!(
        matches!(r[1], Err(ExecuteError::SessionClosed)),
        "{:?}",
        r[1].as_ref().err()
    );
}

#[tokio::test]
async fn sqlite_stops_at_the_first_row_past_a_reached_budget() {
    let fx = sqlite_fixture("", "");
    let mut s = fx.session(SessionOptions::default()).await;
    bounds_hold(&mut s, LITE_HUNDREDS).await;
}

#[tokio::test]
async fn sqlite_delivers_a_first_row_larger_than_the_budget_and_nulls_cost_nothing() {
    let fx = sqlite_fixture("", "");
    let mut s = fx.session(SessionOptions::default()).await;
    let big = "SELECT printf('%05000d', 1) UNION ALL SELECT 'x'";
    assert_eq!(
        shape(&mut s, big, limits(None, Some(100))).await,
        (1, 1, true, Some(TruncatedBy::Bytes))
    );
    let nulls = "WITH RECURSIVE r(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM r WHERE x < 100) SELECT NULL FROM r";
    assert_eq!(
        shape(&mut s, nulls, limits(None, Some(1))).await,
        (100, 100, false, None)
    );
}

#[tokio::test]
async fn sqlite_byte_truncation_under_close_ends_the_session() {
    let fx = sqlite_fixture("", "");
    let mut s = fx.session(SessionOptions::default()).await;
    close_ends_the_session(&mut s, LITE_HUNDREDS).await;
}

#[tokio::test]
async fn postgres_stops_at_the_first_row_past_a_reached_budget() {
    let fx = pg_fixture("").await;
    let mut s = fx.session(SessionOptions::default()).await;
    bounds_hold(&mut s, PG_HUNDREDS).await;
}

#[tokio::test]
async fn postgres_byte_truncation_under_close_ends_the_session() {
    let fx = pg_fixture("").await;
    let mut s = fx.session(SessionOptions::default()).await;
    close_ends_the_session(&mut s, PG_HUNDREDS).await;
}
