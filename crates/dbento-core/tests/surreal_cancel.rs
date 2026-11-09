//! Stopping a stream: `CancelHandle`, the statement deadline, the server's own timeout, and how
//! long a parked result may live.

use std::time::{Duration, Instant};

use crate::support::{run, sr_root, text};
use dbento_core::Session;
use dbento_core::{ExecuteError, Limits, OnTruncate, SessionOptions};
use dbento_testkit::{SURREAL_TAG, surreal_db, surreal_server};

fn parked(max_rows: u64) -> Limits {
    let mut limits = Limits::default();
    limits.max_rows = Some(max_rows);
    limits.on_truncate = OnTruncate::Park;
    limits
}

fn timed(after: Duration) -> SessionOptions {
    let mut opts = SessionOptions::default();
    opts.statement_timeout = Some(after);
    opts
}

fn cancel_after(s: &dbento_core::Session, after: Duration) {
    let handle = s.cancel_handle();
    tokio::spawn(async move {
        tokio::time::sleep(after).await;
        handle.cancel();
    });
}

#[tokio::test]
async fn cancel_stops_a_sleeping_statement_and_the_session_lives() {
    let db = surreal_db().await;
    let mut s = sr_root(&db, "", SessionOptions::default()).await;
    let started = Instant::now();
    cancel_after(&s, Duration::from_millis(300));
    let ran = run(&mut s, "SLEEP 30s", Limits::default()).await;
    assert!(
        matches!(ran[0], Err(ExecuteError::Cancelled)),
        "{:?}",
        ran[0].as_ref().err()
    );
    assert!(started.elapsed() < Duration::from_secs(5));
    let next = run(&mut s, "RETURN 'alive'", Limits::default()).await;
    assert_eq!(next[0].as_ref().unwrap().rows, [vec![text("alive")]]);
}

#[tokio::test]
async fn cancel_mid_stream_ends_the_read() {
    let db = surreal_db().await;
    db.admin("CREATE |r:1..=500| SET a = 1").await;
    let mut s = sr_root(&db, "", SessionOptions::default()).await;
    cancel_after(&s, Duration::from_millis(500));
    let ran = run(
        &mut s,
        "SELECT *, sleep(50ms) AS z FROM r",
        Limits::default(),
    )
    .await;
    assert!(
        matches!(ran[0], Err(ExecuteError::Cancelled)),
        "{:?}",
        ran[0].as_ref().err()
    );
}

#[tokio::test]
async fn cancel_keeps_earlier_statements_of_a_unit_unless_it_is_a_transaction() {
    let db = surreal_db().await;
    let mut s = sr_root(&db, "", SessionOptions::default()).await;
    cancel_after(&s, Duration::from_millis(500));
    let ran = run(
        &mut s,
        "LET $a = 1; CREATE kept:1; SLEEP 30s; CREATE kept:2",
        Limits::default(),
    )
    .await;
    assert!(
        ran.iter()
            .any(|r| matches!(r, Err(ExecuteError::Cancelled)))
    );
    assert_eq!(db.count("kept").await, 1);
    cancel_after(&s, Duration::from_millis(500));
    let ran = run(
        &mut s,
        "BEGIN; CREATE gone:1; SLEEP 30s; CREATE gone:2; COMMIT;",
        Limits::default(),
    )
    .await;
    assert!(
        ran.iter()
            .any(|r| matches!(r, Err(ExecuteError::Cancelled)))
    );
    assert_eq!(db.count("gone").await, 0);
}

#[tokio::test]
async fn a_cancel_with_nothing_running_stops_nothing_later() {
    let db = surreal_db().await;
    let mut s = sr_root(&db, "", SessionOptions::default()).await;
    s.cancel_handle().cancel();
    let ran = run(&mut s, "RETURN 1", Limits::default()).await;
    assert!(ran[0].is_ok());
}

#[tokio::test]
async fn the_deadline_stops_a_statement_without_touching_its_text() {
    let db = surreal_db().await;
    let mut s = sr_root(&db, "", timed(Duration::from_millis(400))).await;
    let started = Instant::now();
    let ran = run(&mut s, "SLEEP 30s", Limits::default()).await;
    assert!(
        matches!(ran[0], Err(ExecuteError::Timeout)),
        "{:?}",
        ran[0].as_ref().err()
    );
    assert!(started.elapsed() < Duration::from_secs(5));
    assert!(run(&mut s, "RETURN 1", Limits::default()).await[0].is_ok());
}

#[tokio::test]
async fn the_servers_own_timeout_is_a_timeout() {
    let db = surreal_server(SURREAL_TAG, &[("SURREAL_QUERY_TIMEOUT", "1s")]).await;
    let mut s = sr_root(&db, "", SessionOptions::default()).await;
    let ran = run(&mut s, "SLEEP 30s", Limits::default()).await;
    assert!(
        matches!(ran[0], Err(ExecuteError::Timeout)),
        "{:?}",
        ran[0].as_ref().err()
    );
    assert!(run(&mut s, "RETURN 1", Limits::default()).await[0].is_ok());
}

#[tokio::test]
async fn cancel_ends_a_parked_read_and_the_session_lives() {
    let db = surreal_db().await;
    db.admin("CREATE |p:1..=2000| SET a = 1").await;
    let mut s = sr_root(&db, "", SessionOptions::default()).await;
    let handle = s.cancel_handle();
    {
        let mut script = s.script("SELECT * FROM p");
        let pending = script.next().await.unwrap().unwrap();
        let mut stream = pending.execute(parked(5)).await.unwrap();
        while stream.next_batch().await.unwrap().is_some() {}
        handle.cancel();
        stream.extend(None).await.unwrap();
        let failed = loop {
            match stream.next_batch().await {
                Ok(Some(_)) => {}
                Ok(None) => break None,
                Err(e) => break Some(e),
            }
        };
        assert!(
            matches!(failed, Some(ExecuteError::Cancelled)),
            "{failed:?}"
        );
    }
    assert!(run(&mut s, "RETURN 'alive'", Limits::default()).await[0].is_ok());
}

// Parks at 5 rows of `SELECT * FROM h`, waits out the server's send timeout, and resumes. Only a
// reader that stops long enough to fill the socket loses its stream, so the rows must exceed the
// 64 MiB buffer and the kernel's.
async fn stalled(s: &mut Session) -> Option<ExecuteError> {
    let mut script = s.script("SELECT * FROM h");
    let pending = script.next().await.unwrap().unwrap();
    let mut stream = pending.execute(parked(5)).await.unwrap();
    while stream.next_batch().await.unwrap().is_some() {}
    tokio::time::sleep(Duration::from_secs(38)).await;
    stream.extend(None).await.unwrap();
    loop {
        match stream.next_batch().await {
            Ok(Some(_)) => {}
            Ok(None) => return None,
            Err(e) => return Some(e),
        }
    }
}

const HEAVY: &str = "CREATE |h:1..=120000| SET a = string::repeat('x', 2000)";

#[tokio::test]
#[ignore = "slow: waits out the server's 30 s send timeout"]
async fn a_stalled_stream_past_the_send_timeout_expires_when_resumed() {
    let db = surreal_db().await;
    db.admin(HEAVY).await;
    let mut s = sr_root(&db, "", SessionOptions::default()).await;
    let failed = stalled(&mut s).await;
    assert!(
        matches!(failed, Some(ExecuteError::ResultExpired)),
        "{failed:?}"
    );
    assert!(run(&mut s, "RETURN 1", Limits::default()).await[0].is_ok());
}

#[tokio::test]
#[ignore = "slow: waits out the server's 30 s send timeout"]
async fn a_stalled_stream_on_a_v4_beta_loses_the_session() {
    let db = surreal_server("v4.0.0-beta.1", &[]).await;
    db.admin(HEAVY).await;
    let mut s = sr_root(&db, "", SessionOptions::default()).await;
    let failed = stalled(&mut s).await.unwrap();
    assert!(failed.closes_session(), "{failed:?}");
}

// Executes the first result of `sql` and drops the script, leaving the rest of its unit to drain.
async fn first_only(s: &mut Session, sql: &str, limits: Limits) {
    let mut script = s.script(sql);
    let pending = script.next().await.unwrap().unwrap();
    let mut stream = pending.execute(limits).await.unwrap();
    while stream.next_batch().await.unwrap().is_some() {}
}

#[tokio::test]
async fn a_deadline_that_stops_an_abandoned_unit_is_owed_to_the_settle() {
    let db = surreal_db().await;
    let mut s = sr_root(&db, "", timed(Duration::from_millis(1500))).await;
    first_only(
        &mut s,
        "LET $a = 1; CREATE owed:1; SLEEP 30s; CREATE owed:2",
        Limits::default(),
    )
    .await;
    let settled = s.settle().await;
    assert!(matches!(settled, Err(ExecuteError::Timeout)), "{settled:?}");
    assert_eq!(db.count("owed").await, 1);
    assert!(run(&mut s, "RETURN 1", Limits::default()).await[0].is_ok());
}

// The lexer sees one statement, the server three: the cut result's `Done` goes out first, and
// the deadline that stops the statements it never saw is heard at the settle.
#[tokio::test]
async fn a_deadline_met_while_a_cut_result_drains_is_owed_to_the_settle() {
    let db = surreal_db().await;
    let mut s = sr_root(&db, "", timed(Duration::from_millis(1500))).await;
    let mut limits = Limits::default();
    limits.max_rows = Some(0);
    let ran = run(&mut s, "RETURN ->? / 2; SLEEP 30s; RETURN 7 / 2", limits).await;
    assert_eq!(ran.len(), 1);
    assert!(ran[0].as_ref().unwrap().outcome.truncated);
    let settled = s.settle().await;
    assert!(matches!(settled, Err(ExecuteError::Timeout)), "{settled:?}");
    assert!(run(&mut s, "RETURN 1", Limits::default()).await[0].is_ok());
}

#[tokio::test]
async fn an_abandoned_read_or_a_cancelled_transaction_owes_nothing() {
    let db = surreal_db().await;
    db.admin("CREATE |q:1..=2000| SET a = 1").await;
    let mut s = sr_root(&db, "", SessionOptions::default()).await;
    // A read nobody reads on is stopped quietly.
    first_only(
        &mut s,
        "LET $a = 1; SELECT * FROM q; SLEEP 30s",
        Limits::default(),
    )
    .await;
    assert!(s.settle().await.is_ok());
    // Every statement of a cancelled transaction is undone, which is not an error.
    first_only(
        &mut s,
        "BEGIN; CREATE undone:1; CREATE undone:2; CANCEL",
        Limits::default(),
    )
    .await;
    assert!(s.settle().await.is_ok());
    assert_eq!(db.count("undone").await, 0);
}

// The lexer reads `RETURN ->? / 2; SLEEP 30s; RETURN 7 / 2` as one statement and the server as
// three. A stop of the `SLEEP` the caller never saw as a statement is heard once, not again at
// the settle.
#[tokio::test]
async fn a_stop_in_a_statement_the_lexer_did_not_see_is_not_owed_again() {
    let db = surreal_db().await;
    let sql = "RETURN ->? / 2; SLEEP 30s; RETURN 7 / 2";
    let mut s = sr_root(&db, "", SessionOptions::default()).await;
    cancel_after(&s, Duration::from_millis(500));
    let ran = run(&mut s, sql, Limits::default()).await;
    assert!(
        matches!(ran[..], [Err(ExecuteError::Cancelled)]),
        "{:?}",
        ran[0].as_ref().err()
    );
    let settled = s.settle().await;
    assert!(settled.is_ok(), "after a cancel: {settled:?}");
    let mut s = sr_root(&db, "", timed(Duration::from_millis(500))).await;
    let ran = run(&mut s, sql, Limits::default()).await;
    assert!(
        matches!(ran[..], [Err(ExecuteError::Timeout)]),
        "{:?}",
        ran[0].as_ref().err()
    );
    let settled = s.settle().await;
    assert!(settled.is_ok(), "after a deadline: {settled:?}");
}
