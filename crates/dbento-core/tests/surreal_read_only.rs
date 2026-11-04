//! The fence: a read-only session runs each unit in a cancelled RPC transaction. Every effect is
//! judged from a separate root connection, and a read-only level is only as good as what that
//! connection sees.
mod support;

use dbento_core::{ExecuteError, Limits, OnTruncate, ReadOnlyEnforcement, SessionOptions, Writes};
use dbento_testkit::{SrDb, surreal_db};
use support::{run, sr_session, text};

const LOGINS: [(&str, &str); 3] = [("ro", "root"), ("ne", "namespace"), ("dv", "database")];

// Stored code the classifier cannot see into, for the fence to undo.
async fn seeded() -> SrDb {
    let db = surreal_db().await;
    db.admin(
        "CREATE vt:1 SET x = 1; CREATE vt:2 SET x = 2;
         DEFINE FUNCTION fn::w() { CREATE fnt:1 SET x = 1; RETURN 1; };
         DEFINE FUNCTION fn::up() { UPDATE vt SET x = 9; RETURN 1; };
         DEFINE FUNCTION fn::c() { CREATE fnc:1 SET x = 1; COMMIT; RETURN 1; };
         DEFINE FUNCTION fn::rm() { REMOVE TABLE vt; RETURN 1; };",
    )
    .await;
    db
}

async fn read_only(db: &SrDb, login: (&str, &str)) -> dbento_core::Session {
    sr_session(
        db,
        Some(login),
        "read_only = true",
        SessionOptions::default(),
    )
    .await
}

async fn tables(db: &SrDb) -> serde_json::Value {
    db.admin("INFO FOR DB").await.pop().unwrap()["tables"].clone()
}

#[tokio::test]
async fn stored_writes_are_undone_for_every_role_and_marked_rolled_back() {
    let db = seeded().await;
    let before = tables(&db).await;
    for login in LOGINS {
        let mut s = read_only(&db, login).await;
        for text_of in ["RETURN fn::w()", "RETURN fn::up()"] {
            let ran = run(&mut s, text_of, Limits::default()).await;
            let ran = ran[0].as_ref().unwrap();
            assert_eq!(ran.rows, [vec![text("1")]], "{login:?}");
            assert!(ran.outcome.rolled_back, "{login:?}: Unknown runs fenced");
        }
        assert_eq!(tables(&db).await, before, "{login:?}");
        assert_eq!(db.scalar("SELECT VALUE x FROM vt:1").await, 1, "{login:?}");
    }
}

// The server refuses a data-modifying statement behind a computed field at read time.
#[tokio::test]
async fn a_computed_field_cannot_write_through_a_plain_select() {
    let db = seeded().await;
    db.admin("DEFINE FIELD c ON vt COMPUTED fn::w()").await;
    let before = tables(&db).await;
    let mut s = read_only(&db, ("ne", "namespace")).await;
    let ran = run(&mut s, "SELECT * FROM vt", Limits::default()).await;
    assert!(
        matches!(ran[0], Err(ExecuteError::Database { .. })),
        "{:?}",
        ran[0].as_ref().err()
    );
    assert_eq!(tables(&db).await, before);
}

#[tokio::test]
async fn text_commit_cannot_end_the_fence() {
    let db = seeded().await;
    for login in [("ro", "root"), ("ne", "namespace")] {
        let mut s = read_only(&db, login).await;
        let ran = run(&mut s, "RETURN fn::c()", Limits::default()).await;
        assert!(ran[0].is_ok(), "{:?}", ran[0].as_ref().err());
        assert_eq!(db.count("fnc").await, 0, "{login:?}");
    }
}

#[tokio::test]
async fn execute_refuses_a_use_then_a_write_then_transaction_control() {
    let db = seeded().await;
    let mut s = read_only(&db, ("ro", "root")).await;
    let ran = run(
        &mut s,
        "LET $a = 1; USE DB app; CREATE leak:1",
        Limits::default(),
    )
    .await;
    assert!(
        matches!(ran[0], Err(ExecuteError::Unsupported { .. })),
        "{:?}",
        ran[0].as_ref().err()
    );
    let ran = run(&mut s, "CREATE leak:1", Limits::default()).await;
    assert!(
        matches!(ran[0], Err(ExecuteError::ReadOnlyViolation { .. })),
        "{:?}",
        ran[0].as_ref().err()
    );
    let ran = run(&mut s, "BEGIN; CREATE leak:1; COMMIT;", Limits::default()).await;
    assert!(
        matches!(ran[0], Err(ExecuteError::ReadOnlyViolation { .. })),
        "a write outranks transaction control: {:?}",
        ran[0].as_ref().err()
    );
    let ran = run(&mut s, "BEGIN; RETURN 1; COMMIT;", Limits::default()).await;
    assert!(
        matches!(ran[0], Err(ExecuteError::TransactionControl { .. })),
        "{:?}",
        ran[0].as_ref().err()
    );
    assert_eq!(db.count("leak").await, 0);
    let ran = run(&mut s, "RETURN 'unharmed'", Limits::default()).await;
    assert_eq!(ran[0].as_ref().unwrap().rows, [vec![text("unharmed")]]);
}

#[tokio::test]
async fn a_read_only_session_can_still_read_a_classification() {
    let db = seeded().await;
    let mut s = read_only(&db, ("ro", "root")).await;
    let mut script = s.script("DELETE vt");
    let pending = script.next().await.unwrap().unwrap();
    assert_eq!(pending.classification().writes, Writes::Yes);
    assert!(!pending.classification().unguarded.is_empty());
}

#[tokio::test]
async fn a_viewer_without_read_only_refuses_a_write_and_names_its_role() {
    let db = seeded().await;
    let mut s = sr_session(&db, Some(("dv", "database")), "", SessionOptions::default()).await;
    assert_eq!(
        s.read_only_enforcement(),
        Some(ReadOnlyEnforcement::Database)
    );
    let ran = run(&mut s, "CREATE leak:1", Limits::default()).await;
    let Err(ExecuteError::ReadOnlyViolation { message }) = &ran[0] else {
        panic!("{:?}", ran[0].as_ref().err());
    };
    assert!(message.contains("VIEWER"), "{message}");
    assert_eq!(
        tables(&db).await.get("leak"),
        None,
        "not even an empty table"
    );
}

#[tokio::test]
async fn an_iam_refusal_in_a_read_only_session_is_a_violation() {
    let db = seeded().await;
    let mut s = read_only(&db, ("dv", "database")).await;
    let ran = run(&mut s, "RETURN fn::rm()", Limits::default()).await;
    let Err(ExecuteError::ReadOnlyViolation { message }) = &ran[0] else {
        panic!("{:?}", ran[0].as_ref().err());
    };
    assert!(message.contains("VIEWER"), "{message}");
}

#[tokio::test]
async fn the_fence_closes_after_an_error_and_after_a_cut() {
    let db = seeded().await;
    db.admin("CREATE |many:1..=2000| SET a = 1").await;
    let mut s = read_only(&db, ("ro", "root")).await;
    let ran = run(
        &mut s,
        "LET $a = 1; RETURN fn::w(); THROW 'boom'; RETURN fn::w()",
        Limits::default(),
    )
    .await;
    assert!(ran.iter().any(Result::is_err));
    let mut cut = Limits::default();
    cut.max_rows = Some(3);
    let ran = run(&mut s, "RETURN fn::w(); SELECT * FROM many", cut).await;
    assert!(ran[1].as_ref().unwrap().outcome.truncated);
    s.settle().await.unwrap();
    assert_eq!(db.count("fnt").await, 0);
}

#[tokio::test]
async fn a_closed_socket_rolls_the_fence_back() {
    let db = seeded().await;
    db.admin("CREATE |many:1..=2000| SET a = 1").await;
    let mut s = read_only(&db, ("ro", "root")).await;
    let mut cut = Limits::default();
    cut.max_rows = Some(3);
    cut.on_truncate = OnTruncate::Close;
    let ran = run(&mut s, "LET $a = fn::w(); SELECT * FROM many", cut).await;
    assert!(ran.iter().all(Result::is_ok));
    assert_eq!(db.count("fnt").await, 0);
}

// The unit keeps the snapshot its `begin` took, which only a fence does: unfenced, the second
// read would see the writer's 50.
#[tokio::test]
async fn a_fenced_unit_reads_one_snapshot_and_a_writer_is_not_blocked() {
    let db = seeded().await;
    let mut s = read_only(&db, ("ro", "root")).await;
    let mut script =
        s.script("LET $a = 1; SELECT VALUE x FROM vt:1; SLEEP 1s; SELECT VALUE x FROM vt:1");
    let mut rows = Vec::new();
    while let Some(next) = script.next().await {
        let ran = support::collect(next.unwrap(), Limits::default())
            .await
            .unwrap();
        rows.extend(ran.rows);
        if rows.len() == 1 && ran.columns.len() == 1 {
            db.admin("UPDATE vt:1 SET x = 50").await;
        }
    }
    assert_eq!(rows, [vec![text("1")], vec![text("1")]]);
    drop(script);
    let ran = run(&mut s, "SELECT VALUE x FROM vt:1", Limits::default()).await;
    assert_eq!(ran[0].as_ref().unwrap().rows, [vec![text("50")]]);
}

// The client's bound on a parked fenced result, shortened through the test seam. Only a fence
// has this bound, so an unfenced session would resume and read every row.
#[tokio::test]
async fn a_parked_fenced_result_expires_at_its_bound_and_the_session_stays() {
    let db = surreal_db().await;
    db.admin("CREATE |p:1..=2000| SET a = 1").await;
    let mut opts = SessionOptions::default();
    opts.force_read_only = true;
    opts.bounds.park = Some(std::time::Duration::from_secs(1));
    let mut s = sr_session(&db, Some(("root", "root")), "", opts).await;
    {
        let mut script = s.script("SELECT * FROM p");
        let pending = script.next().await.unwrap().unwrap();
        let mut stream = pending.execute(parked()).await.unwrap();
        while stream.next_batch().await.unwrap().is_some() {}
        assert!(stream.outcome().unwrap().truncated);
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        stream.extend(None).await.unwrap();
        let failed = stream.next_batch().await.err();
        assert!(
            matches!(failed, Some(ExecuteError::ResultExpired)),
            "{failed:?}"
        );
    }
    assert!(run(&mut s, "RETURN 'alive'", Limits::default()).await[0].is_ok());
}

// Dropped before a row is read: the unit may write, so it runs on, and the fence still closes.
#[tokio::test]
async fn a_fenced_stream_dropped_unread_is_rolled_back_and_the_session_works() {
    let db = seeded().await;
    let mut s = read_only(&db, ("ro", "root")).await;
    {
        let mut script = s.script("RETURN fn::w()");
        let pending = script.next().await.unwrap().unwrap();
        let stream = pending.execute(Limits::default()).await.unwrap();
        drop(stream);
    }
    s.settle().await.unwrap();
    assert_eq!(db.count("fnt").await, 0);
    let ran = run(&mut s, "RETURN 'alive'", Limits::default()).await;
    assert_eq!(ran[0].as_ref().unwrap().rows, [vec![text("alive")]]);
}

// 240 MB of rows, so the socket task has stopped reading at 64 MiB while the result sits parked.
async fn heavy() -> SrDb {
    let db = seeded().await;
    db.admin("CREATE |h:1..=120000| SET a = string::repeat('x', 2000)")
        .await;
    db
}

fn parked() -> Limits {
    let mut limits = Limits::default();
    limits.max_rows = Some(5);
    limits.on_truncate = OnTruncate::Park;
    limits
}

const FILL: std::time::Duration = std::time::Duration::from_secs(2);

#[tokio::test]
async fn a_parked_fenced_read_over_the_buffer_expires_and_its_cancel_gets_through() {
    let db = heavy().await;
    let mut opts = SessionOptions::default();
    opts.force_read_only = true;
    opts.bounds.park = Some(FILL);
    let mut s = sr_session(&db, Some(("ro", "root")), "", opts).await;
    {
        let mut script = s.script("SELECT * FROM h");
        let pending = script.next().await.unwrap().unwrap();
        let mut stream = pending.execute(parked()).await.unwrap();
        while stream.next_batch().await.unwrap().is_some() {}
        tokio::time::sleep(FILL * 2).await;
        stream.extend(None).await.unwrap();
        let failed = stream.next_batch().await.err();
        assert!(
            matches!(failed, Some(ExecuteError::ResultExpired)),
            "{failed:?}"
        );
    }
    let ran = run(&mut s, "RETURN 'alive'", Limits::default()).await;
    assert_eq!(ran[0].as_ref().unwrap().rows, [vec![text("alive")]]);
}

#[tokio::test]
async fn a_cut_over_the_buffer_stops_the_read_and_the_next_call_works() {
    let db = heavy().await;
    let mut s = read_only(&db, ("ro", "root")).await;
    {
        let mut script = s.script("SELECT * FROM h");
        let pending = script.next().await.unwrap().unwrap();
        let mut stream = pending.execute(parked()).await.unwrap();
        while stream.next_batch().await.unwrap().is_some() {}
        tokio::time::sleep(FILL).await;
        drop(stream);
    }
    s.settle().await.unwrap();
    let ran = run(&mut s, "RETURN 'alive'", Limits::default()).await;
    assert_eq!(ran[0].as_ref().unwrap().rows, [vec![text("alive")]]);
    let mut cut = Limits::default();
    cut.max_rows = Some(5);
    let ran = run(&mut s, "SELECT * FROM h", cut).await;
    assert!(ran[0].as_ref().unwrap().outcome.truncated);
    // Still fenced after both, which also makes the cuts above fence cuts.
    let ran = run(&mut s, "RETURN fn::w()", Limits::default()).await;
    assert!(ran[0].as_ref().unwrap().outcome.rolled_back);
    assert_eq!(db.count("fnt").await, 0);
}
