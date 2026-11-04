//! Replies as rows, parked cursors and cancels.
#![cfg(feature = "mongodb")]
mod support;

use std::time::{Duration, Instant};

use bson::doc;
use dbento_core::{
    CellRef, ExecuteError, ExtendError, Limits, OnTruncate, ResultStream, Session, SessionOptions,
    TruncatedBy, Unguarded,
};
use dbento_testkit::{
    MgDb,
    MgUser::{Reader, Writer},
};
use support::{code_of, cursors_back_to, failure, forced, mg, read, text};

fn parked(rows: u64) -> Limits {
    let mut l = Limits::default();
    l.max_rows = Some(rows);
    l.on_truncate = OnTruncate::Park;
    l
}

/// 1,000 documents of 32 KiB. The server caps a reply at 16 MiB, so the first `getMore` stops
/// near document 612 and the cursor stays open; four inserts, as one command cannot pass 16 MiB.
async fn wide(db: &MgDb) {
    let pad = "x".repeat(32 * 1024);
    for start in (0..1_000).step_by(250) {
        let documents: Vec<_> = (start..start + 250)
            .map(|i| doc! { "_id": i, "pad": pad.as_str() })
            .collect();
        db.run(doc! { "insert": "wide", "documents": documents })
            .await;
    }
}

async fn ids(stream: &mut ResultStream<'_>) -> Vec<String> {
    let mut out = Vec::new();
    while let Some(b) = stream.next_batch().await.unwrap() {
        for r in 0..b.len() {
            match b.cell(r, 0) {
                CellRef::Text(t) => out.push(t.to_owned()),
                other => panic!("{other:?}"),
            }
        }
    }
    out
}

async fn cell(s: &mut Session, text: &str, column: &str) -> support::Cell {
    let ran = read(s, text).await;
    let at = ran.columns.iter().position(|(n, _)| n == column).unwrap();
    ran.rows[0][at].clone()
}

#[tokio::test]
async fn counts_and_distinct_are_one_reply_row() {
    let fx = mg("").await;
    let mut s = fx.session(Reader, SessionOptions::default()).await;
    assert_eq!(
        cell(&mut s, "db.many.countDocuments({n: {$lt: 5}})", "n").await,
        text("5")
    );
    assert_eq!(
        cell(&mut s, "db.many.countDocuments({n: -1})", "n").await,
        text("0")
    );
    assert_eq!(
        cell(&mut s, "db.many.estimatedDocumentCount()", "n").await,
        text("10000")
    );
    assert_eq!(
        cell(&mut s, "db.many.distinct('n', {n: {$lt: 2}})", "values").await,
        text("[ 0, 1 ]")
    );
    assert_eq!(
        cell(&mut s, "db.stats()", "db").await,
        text(fx.db.database())
    );
    assert_eq!(
        cell(&mut s, "db.runCommand({ping: 1})", "ok").await,
        text("1")
    );
    let explained = "db.many.aggregate([{$match: {}}], {explain: true})";
    let ran = read(&mut s, explained).await;
    assert!(
        ran.columns.iter().any(|(n, _)| n == "queryPlanner"),
        "{:?}",
        ran.columns
    );
    let ran = read(
        &mut s,
        "db.runCommand({aggregate: 'many', pipeline: [], explain: true})",
    )
    .await;
    assert!(
        ran.columns.iter().any(|(n, _)| n == "queryPlanner"),
        "{:?}",
        ran.columns
    );
}

#[tokio::test]
async fn find_one_and_modify_gives_the_document_before_or_after() {
    let fx = mg("").await;
    let mut s = fx.session(Writer, SessionOptions::default()).await;
    read(&mut s, "db.w.insertOne({_id: 1, n: 1})").await;
    let update = |when: &str| {
        format!(
            "db.w.findOneAndUpdate({{_id: 1}}, {{$inc: {{n: 1}}}}, {{returnDocument: '{when}'}})"
        )
    };
    assert_eq!(cell(&mut s, &update("before"), "n").await, text("1"));
    assert_eq!(cell(&mut s, &update("after"), "n").await, text("3"));
    let ran = read(&mut s, "db.w.findOneAndDelete({_id: 2})").await;
    assert!(ran.rows.is_empty());
    assert_eq!(ran.columns, vec![("_id".to_owned(), String::new())]);
    let ran = read(&mut s, "db.w.findOneAndDelete({_id: 1})").await;
    assert_eq!(ran.rows.len(), 1);
    assert_eq!(fx.db.count("w").await, 0);
}

#[tokio::test]
async fn a_mixed_delete_flags_and_removes_everything() {
    let fx = mg("").await;
    let mut s = fx.session(Writer, SessionOptions::default()).await;
    read(&mut s, "db.w.insertMany([{a: 1}, {a: 1}, {a: 2}])").await;
    let delete =
        "db.runCommand({delete: 'w', deletes: [{q: {a: 1}, limit: 1}, {q: {}, limit: 0}]})";
    {
        let mut script = s.script(delete);
        let p = script.next().await.unwrap().unwrap();
        assert_eq!(
            p.classification().unguarded,
            vec![Unguarded::DeleteAllDocuments]
        );
    }
    assert_eq!(cell(&mut s, delete, "n").await, text("3"));
    assert_eq!(fx.db.count("w").await, 0);
}

#[tokio::test]
async fn a_numeric_multi_is_refused_by_the_server_so_it_needs_no_flag() {
    let fx = mg("").await;
    let mut s = fx.session(Writer, SessionOptions::default()).await;
    let update = "db.runCommand({update: 'many', updates: [{q: {}, u: {$set: {a: 1}}, multi: 1}]})";
    {
        let mut script = s.script(update);
        let p = script.next().await.unwrap().unwrap();
        assert!(p.classification().unguarded.is_empty());
    }
    assert_eq!(code_of(failure(&mut s, update).await), "TypeMismatch");
}

#[tokio::test]
async fn run_command_shows_a_write_error_as_a_reply_row() {
    let fx = mg("").await;
    let mut s = fx.session(Writer, SessionOptions::default()).await;
    let insert = "db.runCommand({insert: 'many', documents: [{_id: 1}]})";
    let ran = read(&mut s, insert).await;
    assert!(
        ran.columns.iter().any(|(n, _)| n == "writeErrors"),
        "{:?}",
        ran.columns
    );
}

#[tokio::test]
async fn an_unknown_command_reaches_the_server_under_database_enforcement() {
    let fx = mg("").await;
    let mut s = fx.session(Reader, forced()).await;
    assert_eq!(
        code_of(failure(&mut s, "db.runCommand({nosuch: 1})").await),
        "CommandNotFound"
    );
    let create = "db.runCommand({createUser: 'x', pwd: 'y', roles: []})";
    assert!(matches!(
        failure(&mut s, create).await,
        ExecuteError::ReadOnlyViolation { .. }
    ));
}

#[tokio::test]
async fn park_then_extend_continues_the_same_cursor() {
    let fx = mg("").await;
    wide(&fx.db).await;
    let baseline = fx.db.open_cursors().await;
    let mut s = fx.session(Reader, SessionOptions::default()).await;
    let mut script = s.script("db.wide.find().sort({_id: 1})");
    let p = script.next().await.unwrap().unwrap();
    let mut stream = p.execute(parked(150)).await.unwrap();
    let first = ids(&mut stream).await;
    assert_eq!((first.len(), first[149].as_str()), (150, "149"));
    assert_eq!(
        stream.outcome().unwrap().truncated_by,
        Some(TruncatedBy::Rows)
    );
    assert_eq!(fx.db.open_cursors().await, baseline + 1);
    stream.extend(Some(150)).await.unwrap();
    let second = ids(&mut stream).await;
    assert_eq!((second.len(), second[0].as_str()), (150, "150"));
    assert_eq!(stream.outcome().unwrap().rows, 300);
    stream.extend(None).await.unwrap();
    assert_eq!(ids(&mut stream).await.len(), 700);
    assert!(!stream.outcome().unwrap().truncated);
    assert!(matches!(
        stream.extend(None).await,
        Err(ExtendError::NotParked)
    ));
    drop(stream);
    cursors_back_to(&fx.db, baseline).await;
}

#[tokio::test]
async fn a_byte_cut_under_park_does_not_park() {
    let fx = mg("").await;
    let baseline = fx.db.open_cursors().await;
    let mut s = fx.session(Reader, SessionOptions::default()).await;
    let mut limits = parked(1_000);
    limits.max_bytes = Some(1);
    let mut script = s.script("db.many.find()");
    let p = script.next().await.unwrap().unwrap();
    let mut stream = p.execute(limits).await.unwrap();
    assert_eq!(ids(&mut stream).await.len(), 1);
    assert_eq!(
        stream.outcome().unwrap().truncated_by,
        Some(TruncatedBy::Bytes)
    );
    assert!(matches!(
        stream.extend(None).await,
        Err(ExtendError::NotParked)
    ));
    cursors_back_to(&fx.db, baseline).await;
}

#[tokio::test]
async fn a_parked_cursor_killed_on_the_server_says_to_run_again() {
    let fx = mg("").await;
    wide(&fx.db).await;
    let mut s = fx.session(Reader, SessionOptions::default()).await;
    let mut script = s.script("db.wide.find()");
    let p = script.next().await.unwrap().unwrap();
    let mut stream = p.execute(parked(150)).await.unwrap();
    ids(&mut stream).await;
    assert_eq!(fx.db.kill_idle_cursors().await, 1);
    stream.extend(None).await.unwrap();
    let e = loop {
        match stream.next_batch().await {
            Ok(Some(_)) => {}
            Ok(None) => panic!("the stream ended without the error"),
            Err(e) => break e,
        }
    };
    match e {
        ExecuteError::Database { code, hint, .. } => {
            assert_eq!(code, "CursorNotFound");
            assert_eq!(
                hint.as_deref(),
                Some("the cursor expired; run the query again")
            );
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn a_cancel_stops_a_running_find_and_the_session_goes_on() {
    let fx = mg("").await;
    // The timeout bounds the server's scan however the dropped call ends. It is far past the
    // cancel at 300 ms, so a cancel that did nothing fails below as `Timeout`, not `Cancelled`.
    let mut o = SessionOptions::default();
    o.statement_timeout = Some(Duration::from_secs(10));
    let mut s = fx.session(Reader, o).await;
    let handle = s.cancel_handle();
    let stop = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        handle.cancel();
    });
    let at = Instant::now();
    let find = "db.many.find({$where: 'sleep(100) || true'})";
    assert!(matches!(
        failure(&mut s, find).await,
        ExecuteError::Cancelled
    ));
    assert!(at.elapsed() < Duration::from_secs(5), "{:?}", at.elapsed());
    stop.await.unwrap();
    s.settle().await.unwrap();
    assert_eq!(read(&mut s, "db.many.countDocuments()").await.rows.len(), 1);
}

#[tokio::test]
async fn a_cancel_during_a_get_more_stops_the_stream_and_leaves_no_cursor() {
    let fx = mg("").await;
    let baseline = fx.db.open_cursors().await;
    let mut s = fx.session(Reader, SessionOptions::default()).await;
    let handle = s.cancel_handle();
    {
        // The first 101 documents are fast, so `execute` returns and the wait is in `getMore`.
        let slow_after_first_batch =
            "db.many.find({$where: 'this._id < 101 || sleep(1000) || true'})";
        let mut script = s.script(slow_after_first_batch);
        let p = script.next().await.unwrap().unwrap();
        let mut stream = p.execute(Limits::default()).await.unwrap();
        let stop = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            handle.cancel();
        });
        let e = loop {
            match stream.next_batch().await {
                Ok(Some(_)) => {}
                Ok(None) => panic!("the stream ended without the cancel"),
                Err(e) => break e,
            }
        };
        assert!(matches!(e, ExecuteError::Cancelled), "{e:?}");
        stop.await.unwrap();
    }
    s.settle().await.unwrap();
    cursors_back_to(&fx.db, baseline).await;
}

#[tokio::test]
async fn an_unspent_cancel_stops_the_next_statement_before_it_is_sent() {
    let fx = mg("").await;
    let mut s = fx.session(Writer, SessionOptions::default()).await;
    s.settle().await.unwrap();
    s.cancel_handle().cancel();
    assert!(matches!(
        failure(&mut s, "db.w.insertOne({a: 1})").await,
        ExecuteError::Cancelled
    ));
    assert_eq!(fx.db.count("w").await, 0);
    s.cancel_handle().cancel();
    s.settle().await.unwrap();
    read(&mut s, "db.w.insertOne({a: 1})").await;
    assert_eq!(fx.db.count("w").await, 1);
}

#[tokio::test]
async fn a_negative_limit_reads_every_document_through_park_and_extend() {
    let fx = mg("").await;
    let mut s = fx.session(Reader, SessionOptions::default()).await;
    let mut script = s.script("db.many.find().sort({_id: 1}).limit(-50)");
    let p = script.next().await.unwrap().unwrap();
    let mut stream = p.execute(parked(5)).await.unwrap();
    assert_eq!(ids(&mut stream).await.len(), 5);
    assert_eq!(
        stream.outcome().unwrap().truncated_by,
        Some(TruncatedBy::Rows)
    );
    stream.extend(None).await.unwrap();
    let rest = ids(&mut stream).await;
    assert_eq!((rest.len(), rest[44].as_str()), (45, "49"));
    let outcome = stream.outcome().unwrap();
    assert_eq!((outcome.rows, outcome.truncated), (50, false));
}

#[tokio::test]
async fn a_batch_size_of_zero_still_reads_every_document() {
    let fx = mg("").await;
    let mut s = fx.session(Reader, SessionOptions::default()).await;
    assert_eq!(
        read(&mut s, "db.many.find().batchSize(0)").await.rows.len(),
        10_000
    );
    let aggregate = "db.many.aggregate([], {batchSize: 0})";
    assert_eq!(read(&mut s, aggregate).await.rows.len(), 10_000);
}

#[tokio::test]
async fn dropping_a_stream_midway_leaves_no_cursor() {
    let fx = mg("").await;
    wide(&fx.db).await;
    let baseline = fx.db.open_cursors().await;
    let mut s = fx.session(Reader, SessionOptions::default()).await;
    let mut script = s.script("db.wide.find()");
    let p = script.next().await.unwrap().unwrap();
    let mut stream = p.execute(Limits::default()).await.unwrap();
    assert!(stream.next_batch().await.unwrap().is_some());
    assert_eq!(fx.db.open_cursors().await, baseline + 1);
    drop(stream);
    cursors_back_to(&fx.db, baseline).await;
}

#[tokio::test]
async fn dropping_a_parked_stream_leaves_no_cursor() {
    let fx = mg("").await;
    wide(&fx.db).await;
    let baseline = fx.db.open_cursors().await;
    let mut s = fx.session(Reader, SessionOptions::default()).await;
    let mut script = s.script("db.wide.find()");
    let p = script.next().await.unwrap().unwrap();
    let mut stream = p.execute(parked(150)).await.unwrap();
    ids(&mut stream).await;
    assert_eq!(fx.db.open_cursors().await, baseline + 1);
    drop(stream);
    cursors_back_to(&fx.db, baseline).await;
}

#[tokio::test]
async fn dropping_a_stream_inside_a_get_more_leaves_no_cursor() {
    let fx = mg("").await;
    let baseline = fx.db.open_cursors().await;
    let mut s = fx.session(Reader, SessionOptions::default()).await;
    {
        let slow_after_first_batch =
            "db.many.find({$where: 'this._id < 101 || sleep(1000) || true'})";
        let mut script = s.script(slow_after_first_batch);
        let p = script.next().await.unwrap().unwrap();
        let mut stream = p.execute(Limits::default()).await.unwrap();
        let waited = tokio::time::timeout(Duration::from_millis(300), ids(&mut stream)).await;
        assert!(waited.is_err(), "the stream ended inside the wait");
        assert_eq!(fx.db.open_cursors().await, baseline + 1);
    }
    cursors_back_to(&fx.db, baseline).await;
}

#[tokio::test]
async fn a_cancel_while_parked_stops_the_resume_and_leaves_no_cursor() {
    let fx = mg("").await;
    wide(&fx.db).await;
    let baseline = fx.db.open_cursors().await;
    let mut s = fx.session(Reader, SessionOptions::default()).await;
    let handle = s.cancel_handle();
    {
        let mut script = s.script("db.wide.find()");
        let p = script.next().await.unwrap().unwrap();
        let mut stream = p.execute(parked(150)).await.unwrap();
        ids(&mut stream).await;
        handle.cancel();
        stream.extend(None).await.unwrap();
        let e = loop {
            match stream.next_batch().await {
                Ok(Some(_)) => {}
                Ok(None) => panic!("the stream ended without the cancel"),
                Err(e) => break e,
            }
        };
        assert!(matches!(e, ExecuteError::Cancelled), "{e:?}");
        cursors_back_to(&fx.db, baseline).await;
    }
    s.settle().await.unwrap();
}
