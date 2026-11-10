use std::sync::Arc;
use std::sync::atomic::Ordering;

use serde_json::{Value, json};

use super::*;
use crate::capacity::CapacityUnits;
use crate::dynamo::replay::{Replay, Reply, page};
use crate::dynamo::statement::Kind;
use crate::session::{
    ExecuteError, ExtendError, Limits, OnTruncate, Outcome, StreamItem, TruncatedBy,
};
use crate::value::{CellRef, ColumnInfo};

pub(super) fn items(range: std::ops::Range<u32>) -> Vec<Value> {
    range
        .map(|n| json!({ "pk": { "S": format!("p{n}") }, "n": { "N": n.to_string() } }))
        .collect()
}

pub(super) fn limits(max_rows: Option<u64>, on_truncate: OnTruncate) -> Limits {
    Limits {
        max_rows,
        max_bytes: None,
        on_truncate,
    }
}

pub(super) async fn open_with(
    replay: &Replay,
    kind: Kind,
    limits: Limits,
    shared: Arc<Shared>,
) -> Result<(Vec<ColumnInfo>, DynamoStream), ExecuteError> {
    let keys = vec!["pk".to_owned()];
    let request = Request {
        kind,
        text: "SELECT * FROM \"t\"".to_owned(),
        consistent_read: false,
    };
    let shape = Shape {
        projection: None,
        keys: &keys,
    };
    DynamoStream::open(
        Arc::new(replay.client(false)),
        request,
        shape,
        limits,
        shared,
    )
    .await
}

pub(super) async fn open(replay: &Replay, limits: Limits) -> (Vec<ColumnInfo>, DynamoStream) {
    open_with(replay, Kind::Select, limits, Arc::new(Shared::new(None)))
        .await
        .unwrap()
}

/// The pk cell of every row until Done, and the outcome.
pub(super) async fn drain(s: &mut DynamoStream) -> (Vec<String>, Outcome) {
    let mut pks = Vec::new();
    loop {
        match s.next_event().await.unwrap() {
            StreamItem::Batch(b) => {
                for r in 0..b.len() {
                    match b.cell(r, 0) {
                        CellRef::Text(t) => pks.push(t.to_owned()),
                        CellRef::Null | CellRef::Bytes(_) => panic!("pk of row {r} is not text"),
                    }
                }
            }
            StreamItem::Done(o) => return (pks, o),
        }
    }
}

fn tokens(replay: &Replay) -> Vec<Option<String>> {
    replay
        .seen()
        .iter()
        .map(|s| {
            s.body
                .get("NextToken")
                .and_then(Value::as_str)
                .map(str::to_owned)
        })
        .collect()
}

fn three_pages() -> Vec<Reply> {
    vec![
        Reply::ok(page(&items(0..2), Some("t1"), Some(0.5))),
        Reply::ok(page(&items(2..4), Some("t2"), Some(1.5))),
        Reply::ok(page(&items(4..6), None, Some(2.0))),
    ]
}

#[tokio::test]
async fn a_limit_stops_after_the_page_that_proves_the_cut() {
    let replay = Replay::new(three_pages());
    let (_, mut s) = open(&replay, limits(Some(3), OnTruncate::Drain)).await;
    let (pks, o) = drain(&mut s).await;
    assert_eq!(pks, ["p0", "p1", "p2"]);
    assert_eq!((o.rows, o.truncated_by), (3, Some(TruncatedBy::Rows)));
    assert_eq!(
        tokens(&replay),
        [None, Some("t1".to_owned())],
        "no third page is requested"
    );
    assert!(matches!(s.extend(None), Err(ExtendError::NotParked)));
}

#[tokio::test]
async fn park_then_extend_resumes_from_the_buffered_row_then_the_token() {
    let replay = Replay::new(three_pages());
    let (_, mut s) = open(&replay, limits(Some(3), OnTruncate::Park)).await;
    let (_, first) = drain(&mut s).await;
    assert_eq!(
        first.consumed_capacity,
        Some(CapacityUnits::from_units(2.0))
    );
    s.extend(Some(10)).unwrap();
    let (pks, o) = drain(&mut s).await;
    assert_eq!(pks, ["p3", "p4", "p5"]);
    assert_eq!((o.rows, o.truncated, o.rows_affected), (6, false, None));
    assert_eq!(o.consumed_capacity, Some(CapacityUnits::from_units(4.0)));
    assert_eq!(
        tokens(&replay),
        [None, Some("t1".to_owned()), Some("t2".to_owned())]
    );
}

#[tokio::test]
async fn close_drops_the_token_and_closes_the_session() {
    let replay = Replay::new(three_pages());
    let shared = Arc::new(Shared::new(None));
    let (_, mut s) = open_with(
        &replay,
        Kind::Select,
        limits(Some(3), OnTruncate::Close),
        Arc::clone(&shared),
    )
    .await
    .unwrap();
    drain(&mut s).await;
    assert!(shared.closed.load(Ordering::Relaxed));
    assert!(matches!(s.extend(None), Err(ExtendError::NotParked)));
}

#[tokio::test]
async fn empty_pages_with_a_token_are_followed_and_unreported_capacity_is_none() {
    let replay = Replay::new(vec![
        Reply::ok(page(&[], Some("t1"), None)),
        Reply::ok(page(&[], Some("t2"), None)),
        Reply::ok(page(&items(0..1), None, None)),
    ]);
    let (_, mut s) = open(&replay, limits(None, OnTruncate::Drain)).await;
    let (pks, o) = drain(&mut s).await;
    assert_eq!((pks, o.consumed_capacity), (vec!["p0".to_owned()], None));
    assert_eq!(replay.seen().len(), 3);
}

#[tokio::test]
async fn capacity_counts_only_the_pages_that_report_it() {
    let replay = Replay::new(vec![
        Reply::ok(page(&items(0..1), Some("t1"), None)),
        Reply::ok(page(&items(1..2), None, Some(1.5))),
    ]);
    let (_, mut s) = open(&replay, limits(None, OnTruncate::Drain)).await;
    assert_eq!(
        drain(&mut s).await.1.consumed_capacity,
        Some(CapacityUnits::from_units(1.5))
    );
}

#[tokio::test]
async fn an_unlimited_fetch_pauses_at_the_batch_cap_and_resumes_without_truncating() {
    // Each item is about 207 bytes by any reasonable estimate, so three fill a 500-byte cap.
    let big = |n: u32| json!({ "pk": { "S": format!("p{n}") }, "pad": { "S": "x".repeat(200) } });
    let replay = Replay::new(
        (0..5u32)
            .map(|n| {
                Reply::ok(page(
                    &[big(n)],
                    (n < 4).then(|| format!("t{}", n + 1)).as_deref(),
                    None,
                ))
            })
            .collect(),
    );
    let shared = Arc::new(Shared::new(None).with_batch_cap(500));
    let (_, mut s) = open_with(
        &replay,
        Kind::Select,
        limits(None, OnTruncate::Drain),
        shared,
    )
    .await
    .unwrap();
    assert_eq!(replay.seen().len(), 3, "the first fetch stops at the cap");
    let (pks, o) = drain(&mut s).await;
    assert_eq!(pks, ["p0", "p1", "p2", "p3", "p4"]);
    assert_eq!((o.rows, o.truncated, o.truncated_by), (5, false, None));
    assert_eq!(replay.seen().len(), 5);
}

#[tokio::test]
async fn a_byte_cut_drops_the_token() {
    let replay = Replay::new(three_pages());
    let byte_limit = Limits {
        max_rows: None,
        max_bytes: Some(1),
        on_truncate: OnTruncate::Park,
    };
    let (_, mut s) = open(&replay, byte_limit).await;
    let (pks, o) = drain(&mut s).await;
    assert_eq!((pks.len(), o.truncated_by), (1, Some(TruncatedBy::Bytes)));
    assert!(matches!(s.extend(None), Err(ExtendError::NotParked)));
    assert_eq!(replay.seen().len(), 1);
}

#[tokio::test]
async fn columns_are_keys_then_sorted_attributes_and_gather_more_while_a_token_remains() {
    let item = json!({ "pk": { "S": "a" }, "z": { "S": "1" }, "b": { "S": "2" } });
    let replay = Replay::new(vec![Reply::ok(page(
        &[item.clone(), item.clone()],
        Some("t1"),
        None,
    ))]);
    let (columns, _) = open(&replay, limits(Some(1), OnTruncate::Drain)).await;
    let names: Vec<&str> = columns.iter().map(|c| c.name()).collect();
    assert_eq!(names, ["pk", "b", "z", "(more attributes)"]);
    let ended = Replay::new(vec![Reply::ok(page(&[item], None, None))]);
    let (columns, _) = open(&ended, limits(None, OnTruncate::Drain)).await;
    assert_eq!(
        columns.iter().map(|c| c.name()).collect::<Vec<_>>(),
        ["pk", "b", "z"]
    );
}

#[tokio::test]
async fn a_write_without_returning_has_no_columns_but_returning_rows_do() {
    let plain = Replay::new(vec![Reply::ok(page(&[], None, None))]);
    let (columns, mut s) = open_with(
        &plain,
        Kind::Insert,
        limits(None, OnTruncate::Drain),
        Arc::new(Shared::new(None)),
    )
    .await
    .unwrap();
    let (pks, o) = drain(&mut s).await;
    assert!(columns.is_empty() && pks.is_empty());
    assert_eq!((o.rows, o.rows_affected), (0, None));
    let returning = Replay::new(vec![Reply::ok(page(&items(0..1), None, None))]);
    let (columns, mut s) = open_with(
        &returning,
        Kind::Update,
        limits(None, OnTruncate::Drain),
        Arc::new(Shared::new(None)),
    )
    .await
    .unwrap();
    assert_eq!(
        columns.iter().map(|c| c.name()).collect::<Vec<_>>(),
        ["pk", "n"]
    );
    assert_eq!(drain(&mut s).await.0, ["p0"]);
}

#[tokio::test]
async fn a_zero_byte_limit_follows_leading_empty_pages_to_the_row_that_proves_the_cut() {
    let replay = Replay::new(vec![
        Reply::ok(page(&[], Some("t1"), None)),
        Reply::ok(page(&[], Some("t2"), None)),
        Reply::ok(page(&items(0..1), None, None)),
    ]);
    let zero_bytes = Limits {
        max_rows: None,
        max_bytes: Some(0),
        on_truncate: OnTruncate::Drain,
    };
    let (_, mut s) = open(&replay, zero_bytes).await;
    let (pks, o) = drain(&mut s).await;
    assert!(pks.is_empty(), "no row fits in zero bytes");
    assert_eq!(o.truncated_by, Some(TruncatedBy::Bytes));
    assert_eq!(replay.seen().len(), 3);
}
