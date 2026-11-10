use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};

use super::Shared;
use super::tests::{drain, items, limits, open, open_with};
use crate::dynamo::errors::STALE_TOKEN_HINT;
use crate::dynamo::replay::{Replay, Reply, aws_error, page};
use crate::dynamo::statement::Kind;
use crate::session::{ExecuteError, OnTruncate, StreamItem};
use crate::value::CellRef;

#[tokio::test]
async fn every_type_decodes_and_the_document_is_the_typed_json() {
    let item = json!({
        "pk": { "S": "t1" }, "a_n": { "N": "12345678901234567890123456789012345678" },
        "b_b": { "B": "AP/+" }, "c_bool": { "BOOL": true }, "d_null": { "NULL": true },
        "e_m": { "M": { "k": { "N": "1" } } }, "f_l": { "L": [{ "NULL": true }, { "S": "a" }] },
        "g_ss": { "SS": ["x", "y"] }, "h_ns": { "NS": ["1", "2"] }, "i_bs": { "BS": ["AQ==", "Ag=="] },
        "j_x": { "XX": "a type newer than the SDK" },
    });
    let replay = Replay::new(vec![Reply::ok(page(
        std::slice::from_ref(&item),
        None,
        None,
    ))]);
    let (columns, mut s) = open(&replay, limits(None, OnTruncate::Drain)).await;
    let types: Vec<&str> = columns.iter().map(|c| c.type_name()).collect();
    assert_eq!(
        types,
        [
            "S", "N", "B", "BOOL", "NULL", "M", "L", "SS", "NS", "BS", "unknown"
        ]
    );
    let StreamItem::Batch(b) = s.next_event().await.unwrap() else {
        panic!("no batch")
    };
    assert_eq!(
        b.cell(0, 1),
        CellRef::Text("12345678901234567890123456789012345678")
    );
    assert!(b.is_number(0, 1));
    assert_eq!(b.cell(0, 2), CellRef::Bytes(&[0, 255, 254]));
    assert_eq!(b.cell(0, 4), CellRef::Null);
    assert_eq!(b.cell(0, 5), CellRef::Text("{'k': 1}"));
    assert_eq!(b.cell(0, 7), CellRef::Text("<<'x', 'y'>>"));
    assert_eq!(b.cell(0, 10), CellRef::Text("(unknown type)"));
    let mut expected = item;
    expected["j_x"] = json!({ "$unknown": true });
    let document: Value =
        serde_json::from_str(&b.document(0).unwrap().to_extended_json(true)).unwrap();
    assert_eq!(document, expected);
}

#[tokio::test]
async fn a_rejected_token_on_a_continuation_says_run_it_again() {
    let replay = Replay::new(vec![
        Reply::ok(page(&items(0..2), Some("t1"), None)),
        Reply::error(
            400,
            aws_error("ValidationException", "The provided token is not valid"),
        ),
    ]);
    let (_, mut s) = open(&replay, limits(Some(1), OnTruncate::Park)).await;
    drain(&mut s).await;
    s.extend(None).unwrap();
    let mut failure = None;
    for _ in 0..3 {
        if let Err(e) = s.next_event().await {
            failure = Some(e);
            break;
        }
    }
    match failure {
        Some(ExecuteError::Database { code, hint, .. }) => {
            assert_eq!(
                (code.as_str(), hint.as_deref()),
                ("ValidationException", Some(STALE_TOKEN_HINT))
            );
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test(start_paused = true)]
async fn the_statement_timeout_bounds_a_whole_fetch_across_pages() {
    let slow = |t: &str| Reply::ok(page(&[], Some(t), None)).after(Duration::from_secs(4));
    let replay = Replay::new(vec![
        slow("t1"),
        slow("t2"),
        slow("t3"),
        slow("t4"),
        slow("t5"),
    ]);
    let shared = Arc::new(Shared::new(Some(Duration::from_secs(10))));
    let failed = open_with(
        &replay,
        Kind::Select,
        limits(None, OnTruncate::Drain),
        shared,
    )
    .await;
    assert!(matches!(failed, Err(ExecuteError::Timeout)));
    assert_eq!(
        replay.seen().len(),
        3,
        "the third page was in flight at ten seconds"
    );
}

#[tokio::test(start_paused = true)]
async fn a_cancel_stops_a_fetch_in_flight_and_an_early_one_sends_nothing() {
    let early = Replay::new(vec![Reply::ok(page(&[], None, None))]);
    let shared = Arc::new(Shared::new(None));
    shared.cancel.cancel();
    let refused = open_with(
        &early,
        Kind::Select,
        limits(None, OnTruncate::Drain),
        Arc::clone(&shared),
    )
    .await;
    assert!(matches!(refused, Err(ExecuteError::Cancelled)));
    assert!(early.seen().is_empty());
    let slow = Replay::new(vec![
        Reply::ok(page(&[], None, None)).after(Duration::from_secs(60)),
    ]);
    let shared = Arc::new(Shared::new(None));
    let canceller = Arc::clone(&shared);
    let (got, ()) = tokio::join!(
        open_with(&slow, Kind::Select, limits(None, OnTruncate::Drain), shared),
        async move {
            tokio::time::sleep(Duration::from_secs(1)).await;
            canceller.cancel.cancel()
        },
    );
    assert!(matches!(got, Err(ExecuteError::Cancelled)));
}
