//! A whole reply under the row and byte limits, parked and extended.
#![cfg(feature = "redis")]
mod support;

use dbento_core::{ExecuteError, Limits, OnTruncate, SessionOptions, TruncatedBy};
use dbento_testkit::{BIG_LIST_LEN, RdUser::Reader};
use support::{failure, one, rd, text};

fn limited(rows: u64, park: bool) -> Limits {
    let mut l = Limits::default();
    l.max_rows = Some(rows);
    if park {
        l.on_truncate = OnTruncate::Park;
    }
    l
}

#[tokio::test]
async fn a_row_limit_truncates_a_whole_reply_without_changing_the_command() {
    let fx = rd("").await;
    let mut s = fx.session(Reader, SessionOptions::default()).await;
    let got = one(&mut s, "LRANGE big:list 0 -1", limited(100, false)).await;
    assert_eq!(
        (got.sql.as_str(), got.rows.len()),
        ("LRANGE big:list 0 -1", 100)
    );
    assert_eq!(got.rows[99], vec![text("99")]);
    assert_eq!(got.outcome.truncated_by, Some(TruncatedBy::Rows));
}

#[tokio::test]
async fn a_byte_limit_cuts_too() {
    let fx = rd("").await;
    let mut s = fx.session(Reader, SessionOptions::default()).await;
    let mut l = Limits::default();
    l.max_bytes = Some(1_000);
    let got = one(&mut s, "LRANGE big:list 0 -1", l).await;
    assert!(got.rows.len() < 1_000, "{}", got.rows.len());
    assert_eq!(got.outcome.truncated_by, Some(TruncatedBy::Bytes));
}

#[tokio::test]
async fn a_parked_reply_extends_to_its_end() {
    let fx = rd("").await;
    let mut s = fx.session(Reader, SessionOptions::default()).await;
    let mut script = s.script("LRANGE big:list 0 -1");
    let p = script.next().await.unwrap().unwrap();
    let mut stream = p.execute(limited(10, true)).await.unwrap();
    let mut rows = 0;
    while let Some(b) = stream.next_batch().await.unwrap() {
        rows += b.len();
    }
    assert_eq!(rows, 10);
    stream.extend(None).await.unwrap();
    while let Some(b) = stream.next_batch().await.unwrap() {
        rows += b.len();
    }
    assert_eq!(rows, BIG_LIST_LEN);
    assert!(!stream.outcome().unwrap().truncated);
}

#[tokio::test]
async fn close_on_truncate_ends_the_session() {
    let fx = rd("").await;
    let mut s = fx.session(Reader, SessionOptions::default()).await;
    let mut l = limited(5, false);
    l.on_truncate = OnTruncate::Close;
    assert_eq!(one(&mut s, "LRANGE big:list 0 -1", l).await.rows.len(), 5);
    let next = failure(&mut s, "GET str").await;
    assert!(next.closes_session(), "{next:?}");
    assert!(!matches!(next, ExecuteError::Timeout));
}
