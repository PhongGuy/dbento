//! Cursors through a session: the column model, limits and the server cursor's end.
#![cfg(feature = "mongodb")]
mod support;

use std::time::Duration;

use bson::doc;
use dbento_core::{CellRef, ExecuteError, Limits, OnTruncate, SessionOptions, TruncatedBy};
use dbento_testkit::{
    MgDb,
    MgUser::{Reader, Root},
};
use support::{Cell, code_of, cursors_back_to, failure, forced, mg, one, read, text};

fn cut(rows: Option<u64>, bytes: Option<u64>, on: OnTruncate) -> Limits {
    let mut l = Limits::default();
    l.max_rows = rows;
    l.max_bytes = bytes;
    l.on_truncate = on;
    l
}

#[tokio::test]
async fn a_find_gives_id_first_and_null_for_an_absent_field() {
    let fx = mg("").await;
    fx.db
        .run(doc! { "insert": "shapes", "documents": [
            { "_id": 1, "b": "x", "a": 2 },
            { "_id": 2, "a": 3 },
        ] })
        .await;
    let mut s = fx.session(Reader, SessionOptions::default()).await;
    let ran = read(&mut s, "db.shapes.find().sort({_id: 1})").await;
    let names: Vec<_> = ran.columns.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["_id", "b", "a"]);
    assert_eq!(ran.rows[1], vec![text("2"), Cell::Null, text("3")]);
    assert_eq!(ran.outcome.rows, 2);
    assert_eq!(ran.outcome.rows_affected, None);
}

#[tokio::test]
async fn no_documents_is_one_id_column_and_no_rows() {
    let fx = mg("").await;
    let mut s = fx.session(Reader, SessionOptions::default()).await;
    let ran = read(&mut s, "db.many.find({n: -1})").await;
    assert_eq!(ran.columns, vec![("_id".to_owned(), String::new())]);
    assert!(ran.rows.is_empty());
    assert!(!ran.outcome.truncated);
}

#[tokio::test]
async fn a_row_limit_truncates_and_kills_the_cursor() {
    let fx = mg("").await;
    let baseline = fx.db.open_cursors().await;
    let mut s = fx.session(Reader, SessionOptions::default()).await;
    let ran = one(
        &mut s,
        "db.many.find()",
        cut(Some(100), None, OnTruncate::Drain),
    )
    .await;
    assert_eq!(ran.rows.len(), 100);
    assert_eq!(ran.outcome.truncated_by, Some(TruncatedBy::Rows));
    cursors_back_to(&fx.db, baseline).await;
}

#[tokio::test]
async fn an_exact_fit_is_not_truncated_and_the_users_limit_stands() {
    let fx = mg("").await;
    let mut s = fx.session(Reader, SessionOptions::default()).await;
    let ran = one(
        &mut s,
        "db.many.find().limit(3)",
        cut(Some(3), None, OnTruncate::Drain),
    )
    .await;
    assert_eq!((ran.rows.len(), ran.outcome.truncated), (3, false));
    let ran = one(
        &mut s,
        "db.many.find().limit(3)",
        cut(Some(100), None, OnTruncate::Drain),
    )
    .await;
    assert_eq!((ran.rows.len(), ran.outcome.truncated), (3, false));
}

#[tokio::test]
async fn a_read_past_the_first_batch_gets_every_document_in_batches() {
    let fx = mg("").await;
    let mut s = fx.session(Reader, SessionOptions::default()).await;
    let ran = read(&mut s, "db.many.find().sort({_id: 1})").await;
    assert_eq!(ran.rows.len(), 10_000);
    assert_eq!(ran.rows[9_999][0], text("9999"));
    assert!(!ran.outcome.truncated);
}

#[tokio::test]
async fn a_byte_limit_still_gives_one_document_and_counts_the_raw_bytes() {
    let fx = mg("").await;
    let mut s = fx.session(Reader, SessionOptions::default()).await;
    let ran = one(
        &mut s,
        "db.many.find()",
        cut(None, Some(1), OnTruncate::Drain),
    )
    .await;
    assert_eq!(ran.rows.len(), 1);
    assert_eq!(ran.outcome.truncated_by, Some(TruncatedBy::Bytes));
    // A row costs its cells plus its raw document, and `Limits::cut` stops the first row that
    // finds the total at or past the limit. An `_id` of d digits is 33 + d bytes of BSON and
    // 4 + 3d of cells (`(more fields)` is null), so ids 0 to 9 cost 41 and ids 10 to 99 cost 45:
    // ten rows total 410, eleven 455, twelve 500.
    let sorted = "db.many.find().sort({_id: 1})";
    for (limit, rows) in [(410, 10), (411, 11), (455, 11), (456, 12)] {
        let ran = one(&mut s, sorted, cut(None, Some(limit), OnTruncate::Drain)).await;
        assert_eq!(ran.rows.len(), rows, "limit {limit}");
        assert_eq!(ran.outcome.truncated_by, Some(TruncatedBy::Bytes));
    }
}

#[tokio::test]
async fn a_field_after_the_buffer_goes_to_more_fields() {
    let fx = mg("").await;
    let early: Vec<_> = (0..101).map(|i| doc! { "_id": i, "a": i }).collect();
    let late: Vec<_> = (101..150)
        .map(|i| doc! { "_id": i, "a": i, "b": "later" })
        .collect();
    fx.db
        .run(doc! { "insert": "shapes", "documents": early })
        .await;
    fx.db
        .run(doc! { "insert": "shapes", "documents": late })
        .await;
    let mut s = fx.session(Reader, SessionOptions::default()).await;
    let ran = read(&mut s, "db.shapes.find().sort({_id: 1})").await;
    assert_eq!(ran.columns.last().unwrap().0, "(more fields)");
    assert_eq!(ran.rows[0][2], Cell::Null);
    assert_eq!(ran.rows[149][2], text("{ b: 'later' }"));
}

#[tokio::test]
async fn close_ends_the_session_at_the_cut() {
    let fx = mg("").await;
    let baseline = fx.db.open_cursors().await;
    let mut s = fx.session(Reader, SessionOptions::default()).await;
    let ran = one(
        &mut s,
        "db.many.find()",
        cut(Some(3), None, OnTruncate::Close),
    )
    .await;
    assert_eq!((ran.rows.len(), ran.outcome.truncated), (3, true));
    cursors_back_to(&fx.db, baseline).await;
    assert!(matches!(
        failure(&mut s, "db.many.find()").await,
        ExecuteError::SessionClosed
    ));
}

#[tokio::test]
async fn a_slow_find_stops_at_the_statement_timeout() {
    let fx = mg("").await;
    let mut o = SessionOptions::default();
    o.statement_timeout = Some(std::time::Duration::from_millis(50));
    let mut s = fx.session(Reader, o).await;
    // 10,000 documents at 10 ms each need 100 s, so 50 ms always fires first.
    let find = "db.many.find({$where: 'sleep(10) || true'})";
    assert!(matches!(failure(&mut s, find).await, ExecuteError::Timeout));
}

/// A capped collection of `documents`, which a tailable find can follow past its end.
async fn capped_log(db: &MgDb, documents: i32) {
    db.run(doc! { "create": "log", "capped": true, "size": 1 << 20 })
        .await;
    let rows: Vec<_> = (0..documents).map(|i| doc! { "_id": i }).collect();
    db.run(doc! { "insert": "log", "documents": rows }).await;
}

// The outer bound keeps a statement that never ends from hanging the test binary.
async fn bounded<F: Future>(f: F) -> F::Output {
    tokio::time::timeout(Duration::from_secs(6), f)
        .await
        .expect("the statement outlived its timeout")
}

#[tokio::test]
async fn a_tailable_find_stops_at_the_statement_timeout_and_leaves_no_cursor() {
    let fx = mg("").await;
    capped_log(&fx.db, 3).await;
    let baseline = fx.db.open_cursors().await;
    let mut o = SessionOptions::default();
    o.statement_timeout = Some(Duration::from_millis(100));
    let mut s = fx.session(Reader, o).await;
    for find in [
        "db.runCommand({find: 'log', tailable: true, awaitData: true})",
        "db.runCommand({find: 'log', tailable: true})",
    ] {
        let stopped = bounded(failure(&mut s, find)).await;
        assert!(matches!(stopped, ExecuteError::Timeout), "{stopped:?}");
        cursors_back_to(&fx.db, baseline).await;
    }
    assert_eq!(read(&mut s, "db.log.countDocuments()").await.rows.len(), 1);
}

// Past the buffer the wait moves from `execute` to the stream.
#[tokio::test]
async fn a_tailable_find_past_its_buffer_stops_at_the_statement_timeout() {
    let fx = mg("").await;
    capped_log(&fx.db, 150).await;
    let baseline = fx.db.open_cursors().await;
    let mut o = SessionOptions::default();
    o.statement_timeout = Some(Duration::from_millis(100));
    let mut s = fx.session(Reader, o).await;
    let mut script = s.script("db.runCommand({find: 'log', tailable: true, awaitData: true})");
    let p = script.next().await.unwrap().unwrap();
    let mut stream = bounded(p.execute(Limits::default())).await.unwrap();
    let stopped = bounded(async {
        loop {
            if let Err(e) = stream.next_batch().await {
                break e;
            }
        }
    })
    .await;
    assert!(matches!(stopped, ExecuteError::Timeout), "{stopped:?}");
    drop(stream);
    cursors_back_to(&fx.db, baseline).await;
}

#[tokio::test]
async fn a_tailable_find_with_no_timeout_waits_for_a_cancel() {
    let fx = mg("").await;
    capped_log(&fx.db, 3).await;
    let mut s = fx.session(Reader, SessionOptions::default()).await;
    let handle = s.cancel_handle();
    let stop = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(1_500)).await;
        handle.cancel();
    });
    let find = "db.runCommand({find: 'log', tailable: true, awaitData: true})";
    let stopped = bounded(failure(&mut s, find)).await;
    assert!(matches!(stopped, ExecuteError::Cancelled), "{stopped:?}");
    stop.await.unwrap();
}

#[tokio::test]
async fn a_read_on_another_database_stays_unauthorized_for_a_reader() {
    let fx = mg("").await;
    let mut s = fx.session(Reader, forced()).await;
    assert_eq!(
        code_of(failure(&mut s, "db.getSiblingDB('elsewhere').t.find()").await),
        "Unauthorized"
    );
}

#[tokio::test]
async fn find_one_aggregate_and_the_listing_methods_are_cursors() {
    let fx = mg("").await;
    let mut s = fx.session(Root, SessionOptions::default()).await;
    let ran = read(&mut s, "db.many.findOne({_id: 7})").await;
    assert_eq!(ran.rows[0][0], text("7"));
    let ran = read(
        &mut s,
        "db.many.aggregate([{$match: {_id: 7}}, {$project: {_id: 0, n: 1}}])",
    )
    .await;
    assert_eq!(ran.columns[0].0, "n");
    let ran = read(&mut s, "db.many.getIndexes()").await;
    assert!(
        ran.columns.iter().any(|(n, _)| n == "key"),
        "{:?}",
        ran.columns
    );
    let ran = read(&mut s, "db.getCollectionNames()").await;
    assert_eq!(ran.rows.len(), 2);
}

#[tokio::test]
async fn a_raw_find_goes_through_a_cursor_and_leaves_none_open() {
    let fx = mg("").await;
    let baseline = fx.db.open_cursors().await;
    let mut s = fx.session(Reader, SessionOptions::default()).await;
    let ran = one(
        &mut s,
        "db.runCommand({find: 'many'})",
        cut(Some(5), None, OnTruncate::Drain),
    )
    .await;
    assert_eq!(ran.rows.len(), 5);
    assert_eq!(ran.columns[0].0, "_id");
    cursors_back_to(&fx.db, baseline).await;
}

#[tokio::test]
async fn a_result_that_ends_just_as_the_buffer_fills_is_not_truncated() {
    let fx = mg("").await;
    let mut s = fx.session(Reader, SessionOptions::default()).await;
    let ran = read(&mut s, "db.many.find().limit(101)").await;
    assert_eq!((ran.rows.len(), ran.outcome.truncated), (101, false));
    // The end was not yet known when the columns were fixed, so the column exists, empty.
    assert_eq!(ran.columns.last().unwrap().0, "(more fields)");
    assert!(ran.rows.iter().all(|row| row.last() == Some(&Cell::Null)));
}

#[tokio::test]
async fn a_single_batch_find_is_cut_or_read_whole_under_a_row_limit() {
    let fx = mg("").await;
    let mut s = fx.session(Reader, SessionOptions::default()).await;
    let find = "db.many.find().limit(-50)";
    let ran = one(&mut s, find, cut(Some(3), None, OnTruncate::Drain)).await;
    assert_eq!(ran.rows.len(), 3);
    assert_eq!(ran.outcome.truncated_by, Some(TruncatedBy::Rows));
    let ran = one(&mut s, find, cut(Some(50), None, OnTruncate::Drain)).await;
    assert_eq!((ran.rows.len(), ran.outcome.truncated), (50, false));
}

/// Reads `text` to its end and checks each row's document is that row's, batch by batch.
async fn check_documents(s: &mut dbento_core::Session, text: &str) -> (usize, usize) {
    let mut script = s.script(text);
    let p = script.next().await.unwrap().unwrap();
    let mut stream = p.execute(Limits::default()).await.unwrap();
    let (mut rows, mut batches) = (0, 0);
    while let Some(batch) = stream.next_batch().await.unwrap() {
        batches += 1;
        for r in 0..batch.len() {
            assert_eq!(batch.cell(r, 0), CellRef::Text(&rows.to_string()));
            let shell = batch
                .document(r)
                .expect("a document per row")
                .to_shell(false);
            assert!(
                shell.starts_with(&format!("{{ _id: {rows},")),
                "row {rows}: {shell:.40}"
            );
            rows += 1;
        }
        assert!(batch.document(batch.len()).is_none());
    }
    assert_eq!(stream.outcome().unwrap().rows, rows as u64);
    (rows, batches)
}

#[tokio::test]
async fn each_row_keeps_its_own_document_across_every_batch_close() {
    let fx = mg("").await;
    let mut s = fx.session(Reader, SessionOptions::default()).await;
    // 1,024 rows close a batch here.
    let (rows, batches) = check_documents(&mut s, "db.many.find().sort({_id: 1})").await;
    assert_eq!(rows, 10_000);
    assert!(batches >= 10, "{batches}");
    // 8 MiB closes a batch here, on the push that would pass it. A row counts its pad twice,
    // in its cell and its document, so three fit.
    let pad = "x".repeat(1 << 20);
    let big: Vec<_> = (0..12)
        .map(|i| doc! { "_id": i, "pad": pad.as_str() })
        .collect();
    fx.db.run(doc! { "insert": "big", "documents": big }).await;
    let (rows, batches) = check_documents(&mut s, "db.big.find().sort({_id: 1})").await;
    assert_eq!((rows, batches), (12, 4));
}
