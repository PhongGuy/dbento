//! Row and byte limits: `Drain`, `Close` and `Park` for reads and for statements that may write,
//! a dropped stream, and the drain bound.
mod support;

use std::time::{Duration, Instant};

use dbento_core::{
    Engine, ExecuteError, ExtendError, Limits, OnTruncate, SessionOptions, TruncatedBy,
};
use dbento_testkit::{SrDb, surreal_db};
use support::{collect, run, sr_root, text};

fn limits(max_rows: u64, on_truncate: OnTruncate) -> Limits {
    let mut limits = Limits::default();
    limits.max_rows = Some(max_rows);
    limits.on_truncate = on_truncate;
    limits
}

async fn seeded() -> SrDb {
    let db = surreal_db().await;
    db.admin("CREATE |big:1..=2000| SET name = 'row'").await;
    db
}

#[tokio::test]
async fn a_drain_cut_marks_the_result_and_keeps_the_session() {
    let db = seeded().await;
    let mut s = sr_root(&db, "", SessionOptions::default()).await;
    let ran = run(&mut s, "SELECT * FROM big", limits(10, OnTruncate::Drain)).await;
    let ran = ran[0].as_ref().unwrap();
    assert_eq!(ran.rows.len(), 10);
    assert!(ran.outcome.truncated);
    assert_eq!(ran.outcome.truncated_by, Some(TruncatedBy::Rows));
    s.settle().await.unwrap();
    let next = run(&mut s, "RETURN 'alive'", Limits::default()).await;
    assert_eq!(next[0].as_ref().unwrap().rows, [vec![text("alive")]]);
}

#[tokio::test]
async fn a_result_of_exactly_the_limit_is_not_truncated() {
    let db = surreal_db().await;
    db.admin("CREATE e:1; CREATE e:2").await;
    let mut s = sr_root(&db, "", SessionOptions::default()).await;
    let ran = run(&mut s, "SELECT * FROM e", limits(2, OnTruncate::Drain)).await;
    let ran = ran[0].as_ref().unwrap();
    assert_eq!(ran.rows.len(), 2);
    assert!(!ran.outcome.truncated);
}

#[tokio::test]
async fn a_byte_cut_is_named_as_bytes() {
    let db = seeded().await;
    let mut s = sr_root(&db, "", SessionOptions::default()).await;
    let mut cap = Limits::default();
    cap.max_bytes = Some(100);
    let ran = run(&mut s, "SELECT * FROM big", cap).await;
    let ran = ran[0].as_ref().unwrap();
    assert_eq!(ran.outcome.truncated_by, Some(TruncatedBy::Bytes));
    assert!(ran.rows.len() < 2000);
}

// A read past the cut is stopped rather than read through: its table could outlast the bound.
#[tokio::test]
async fn a_drain_cut_on_a_lone_read_stops_the_statement() {
    let db = surreal_db().await;
    db.admin("CREATE |slow:1..=8000| SET name = 'row'").await;
    let mut s = sr_root(&db, "", SessionOptions::default()).await;
    let started = Instant::now();
    let ran = run(
        &mut s,
        "SELECT * FROM slow WHERE sleep(2ms) = NONE",
        limits(1, OnTruncate::Drain),
    )
    .await;
    assert!(ran[0].as_ref().unwrap().outcome.truncated);
    s.settle().await.unwrap();
    // The first frame takes a second and the whole table 16.
    assert!(
        started.elapsed() < Duration::from_secs(8),
        "{:?}",
        started.elapsed()
    );
    assert!(run(&mut s, "RETURN 1", Limits::default()).await[0].is_ok());
}

#[tokio::test]
async fn a_cut_inside_a_unit_still_delivers_its_later_statements() {
    let db = seeded().await;
    let mut s = sr_root(&db, "", SessionOptions::default()).await;
    let ran = run(
        &mut s,
        "LET $a = 1; SELECT * FROM big; RETURN 'then';",
        limits(5, OnTruncate::Drain),
    )
    .await;
    assert_eq!(ran.len(), 3);
    assert!(ran[1].as_ref().unwrap().outcome.truncated);
    assert_eq!(ran[2].as_ref().unwrap().rows, [vec![text("then")]]);
}

#[tokio::test]
async fn close_on_a_read_closes_the_session() {
    let db = seeded().await;
    let mut s = sr_root(&db, "", SessionOptions::default()).await;
    let ran = run(&mut s, "SELECT * FROM big", limits(5, OnTruncate::Close)).await;
    assert!(ran[0].as_ref().unwrap().outcome.truncated);
    let next = run(&mut s, "RETURN 1", Limits::default()).await;
    assert!(
        matches!(next[0], Err(ExecuteError::SessionClosed)),
        "{:?}",
        next[0].as_ref().err()
    );
}

// Stopping a write would lose the rows it had not reached, so `Close` drains it.
#[tokio::test]
async fn close_on_a_write_finishes_the_write_and_keeps_the_session() {
    let db = surreal_db().await;
    let mut s = sr_root(&db, "", SessionOptions::default()).await;
    let ran = run(
        &mut s,
        "CREATE |w:1..=1000| SET a = 1",
        limits(5, OnTruncate::Close),
    )
    .await;
    assert!(ran[0].as_ref().unwrap().outcome.truncated);
    s.settle().await.unwrap();
    assert_eq!(db.count("w").await, 1000);
    assert!(run(&mut s, "RETURN 1", Limits::default()).await[0].is_ok());
}

#[tokio::test]
async fn park_holds_the_read_and_extend_resumes_it() {
    let db = seeded().await;
    let mut s = sr_root(&db, "", SessionOptions::default()).await;
    let mut script = s.script("SELECT * FROM big");
    let pending = script.next().await.unwrap().unwrap();
    let mut stream = pending.execute(limits(10, OnTruncate::Park)).await.unwrap();
    let mut rows = 0;
    while let Some(batch) = stream.next_batch().await.unwrap() {
        rows += batch.len();
    }
    assert_eq!(rows, 10);
    assert_eq!(
        stream.outcome().unwrap().truncated_by,
        Some(TruncatedBy::Rows)
    );
    stream.extend(Some(15)).await.unwrap();
    while let Some(batch) = stream.next_batch().await.unwrap() {
        rows += batch.len();
    }
    assert_eq!(rows, 25);
    stream.extend(None).await.unwrap();
    while let Some(batch) = stream.next_batch().await.unwrap() {
        rows += batch.len();
    }
    assert_eq!(rows, 2000);
    assert!(!stream.outcome().unwrap().truncated);
    assert!(matches!(
        stream.extend(None).await,
        Err(ExtendError::NotParked)
    ));
}

#[tokio::test]
async fn park_on_a_write_drains_and_cannot_be_extended() {
    let db = surreal_db().await;
    let mut s = sr_root(&db, "", SessionOptions::default()).await;
    let mut script = s.script("CREATE |w:1..=1000| SET a = 1");
    let pending = script.next().await.unwrap().unwrap();
    let mut stream = pending.execute(limits(5, OnTruncate::Park)).await.unwrap();
    while stream.next_batch().await.unwrap().is_some() {}
    assert!(stream.outcome().unwrap().truncated);
    assert!(matches!(
        stream.extend(None).await,
        Err(ExtendError::NotParked)
    ));
    drop(stream);
    drop(script);
    s.settle().await.unwrap();
    assert_eq!(db.count("w").await, 1000);
}

#[tokio::test]
async fn a_dropped_stream_stops_a_read_and_lets_a_write_finish() {
    let db = seeded().await;
    let mut s = sr_root(&db, "", SessionOptions::default()).await;
    {
        let mut script = s.script("SELECT * FROM big");
        let pending = script.next().await.unwrap().unwrap();
        let stream = pending.execute(Limits::default()).await.unwrap();
        drop(stream);
    }
    s.settle().await.unwrap();
    {
        let mut script = s.script("CREATE |w:1..=1000| SET a = 1");
        let pending = script.next().await.unwrap().unwrap();
        let stream = pending.execute(Limits::default()).await.unwrap();
        drop(stream);
    }
    s.settle().await.unwrap();
    assert_eq!(db.count("w").await, 1000);
    assert!(run(&mut s, "RETURN 1", Limits::default()).await[0].is_ok());
}

// Nobody reads the rest of a unit that may write, so it drains, and a `SLEEP` outlives the bound,
// shortened here: the socket goes and stops it there, and the statement before it stands.
#[tokio::test]
async fn a_unit_past_the_drain_bound_closes_the_session_and_earlier_statements_stand() {
    let db = surreal_db().await;
    let mut opts = SessionOptions::default();
    opts.bounds.drain = Some(Duration::from_secs(1));
    let mut s = sr_root(&db, "", opts).await;
    let mut script = s.script("LET $a = 1; CREATE early:1; SLEEP 30s; CREATE late:1;");
    for _ in 0..2 {
        let pending = script.next().await.unwrap().unwrap();
        collect(pending, Limits::default()).await.unwrap();
    }
    drop(script);
    let started = Instant::now();
    let settled = s.settle().await;
    assert!(
        matches!(settled, Err(ExecuteError::StoppedAtDrainCap)),
        "{settled:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(5));
    let words = ExecuteError::StoppedAtDrainCap.message_for(Engine::Surrealdb);
    assert!(words.contains("earlier statements"), "{words}");
    assert_eq!(db.count("early").await, 1);
    assert_eq!(
        db.count("late").await,
        0,
        "the closed socket stopped the rest"
    );
    let next = run(&mut s, "RETURN 1", Limits::default()).await;
    assert!(
        next[0]
            .as_ref()
            .err()
            .is_some_and(ExecuteError::closes_session)
    );
}
