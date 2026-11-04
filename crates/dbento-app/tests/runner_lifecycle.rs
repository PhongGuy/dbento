//! Truncation policy, settling, reconnecting, and the panic path of the runner.
mod support;
use dbento_app::runner::{
    Command, Event, RunnerTuning, SessionSlot, StatementEnd, guarded, spawn_tuned,
};
use std::sync::Arc;
use std::time::Duration;
use support::{MANY, fixture, here, next_event, pg_spec, run_cmd, spec, until_event};

const SLOW: &str = "SELECT repeat('x', 10000), pg_sleep(0.1) FROM generate_series(1, 300)";
const DIV: &str = "SELECT 1 / (x - 3000) FROM generate_series(1, 5000) x";
fn tuning() -> RunnerTuning {
    RunnerTuning {
        idle_complete: Duration::from_secs(60),
        park_at_cap: Duration::from_millis(200),
        ..Default::default()
    }
}
fn has(seen: &[Event], f: impl Fn(&Event) -> bool) -> bool {
    seen.iter().any(f)
}
async fn run_done(ev: &async_channel::Receiver<Event>, id: u64) -> Vec<Event> {
    until_event(
        ev,
        |e| matches!(e, Event::RunDone { run_id } if *run_id == id),
    )
    .await
    .0
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cut_short_read_only_result_disconnects_and_the_next_run_reconnects() {
    let fx = fixture("", "read_only = true");
    let r = spawn_tuned(&here(), spec(&fx, "lite"), tuning());
    let ev = r.events();
    r.send(run_cmd(1, MANY, Some(1000)));
    let seen = run_done(&ev, 1).await;
    assert!(has(&seen, |e| matches!(
        e,
        Event::StatementDone {
            end: StatementEnd::Truncated(_),
            ..
        }
    )));
    assert!(
        has(&seen, |e| matches!(e, Event::Disconnected { .. })),
        "{seen:?}"
    );
    r.send(run_cmd(2, "SELECT 1", Some(1000)));
    assert!(matches!(
        next_event(&ev).await,
        Event::Connected {
            read_only: true,
            ..
        }
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn stopping_a_parked_read_only_stream_disconnects() {
    let fx = fixture("", "read_only = true");
    let r = spawn_tuned(&here(), spec(&fx, "lite"), tuning());
    let ev = r.events();
    r.send(run_cmd(1, MANY, Some(10_000)));
    until_event(&ev, |e| matches!(e, Event::Paused { .. })).await;
    r.send(Command::Stop { run_id: 1 });
    assert!(has(&run_done(&ev, 1).await, |e| matches!(
        e,
        Event::Disconnected { .. }
    )));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_read_write_session_settles_and_survives_a_cut_short_result() {
    let fx = fixture("", "");
    let r = spawn_tuned(&here(), spec(&fx, "lite"), tuning());
    let ev = r.events();
    r.send(run_cmd(1, MANY, Some(1000)));
    let seen = run_done(&ev, 1).await;
    assert!(
        has(&seen, |e| matches!(e, Event::Settling { run_id: 1 })),
        "{seen:?}"
    );
    assert!(!has(&seen, |e| matches!(
        e,
        Event::Disconnected { .. } | Event::SettleFailed { .. }
    )));
    r.send(run_cmd(2, "SELECT 1", Some(1000)));
    assert!(
        matches!(next_event(&ev).await, Event::Executing { run_id: 2, .. }),
        "no reconnect"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_drain_error_is_reported() {
    let (_db, _dir, spec) = pg_spec("").await;
    let r = spawn_tuned(&here(), spec, tuning());
    let ev = r.events();
    r.send(run_cmd(1, DIV, Some(10)));
    let seen = run_done(&ev, 1).await;
    assert!(
        has(
            &seen,
            |e| matches!(e, Event::SettleFailed { message } if message.contains("division by zero"))
        ),
        "{seen:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn cancel_stops_a_drain_and_keeps_the_session() {
    let (_db, _dir, spec) = pg_spec("").await;
    let r = spawn_tuned(&here(), spec, tuning());
    let ev = r.events();
    r.send(run_cmd(1, SLOW, Some(1)));
    let Event::Connected { cancel, .. } = next_event(&ev).await else {
        panic!("not connected")
    };
    until_event(&ev, |e| matches!(e, Event::Settling { run_id: 1 })).await;
    cancel.cancel();
    let seen = run_done(&ev, 1).await;
    assert!(
        has(
            &seen,
            |e| matches!(e, Event::SettleFailed { message } if message.contains("cancelled"))
        ),
        "{seen:?}"
    );
    r.send(run_cmd(2, "SELECT 1", Some(10)));
    assert!(
        matches!(next_event(&ev).await, Event::Executing { run_id: 2, .. }),
        "no reconnect"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_drain_past_the_cap_disconnects() {
    let (_db, _dir, spec) = pg_spec("").await;
    let r = spawn_tuned(&here(), spec, tuning());
    let ev = r.events();
    r.send(run_cmd(1, SLOW, Some(1)));
    let seen = run_done(&ev, 1).await;
    assert!(
        has(
            &seen,
            |e| matches!(e, Event::Disconnected { reason } if reason.contains("rolled back"))
        ),
        "{seen:?}"
    );
    r.send(run_cmd(2, "SELECT 1", Some(10)));
    assert!(matches!(next_event(&ev).await, Event::Connected { .. }));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_read_only_postgres_cut_leaves_no_open_transaction() {
    let (db, _dir, spec) = pg_spec("read_only = true").await;
    let r = spawn_tuned(&here(), spec, tuning());
    let ev = r.events();
    r.send(run_cmd(
        1,
        "SELECT x FROM generate_series(1, 100000) x",
        Some(10),
    ));
    assert!(has(&run_done(&ev, 1).await, |e| matches!(
        e,
        Event::Disconnected { .. }
    )));
    let open = "SELECT count(*) FROM pg_stat_activity WHERE application_name = 'dbento' AND datname = current_database() AND state LIKE 'idle in transaction%'";
    assert_eq!(db.scalar(open).await.as_deref(), Some("0"));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_lost_idle_connection_disconnects_without_a_run_and_the_next_run_reconnects() {
    let (db, _dir, spec) = pg_spec("").await;
    let r = spawn_tuned(&here(), spec, tuning());
    let ev = r.events();
    r.send(run_cmd(1, "SELECT 1", Some(10)));
    run_done(&ev, 1).await;
    db.scalar("SELECT count(pg_terminate_backend(pid)) FROM pg_stat_activity WHERE application_name = 'dbento' AND datname = current_database()").await;
    // No command is sent: the idle runner must notice the dead session by itself.
    assert!(matches!(next_event(&ev).await, Event::Disconnected { .. }));
    r.send(run_cmd(2, "SELECT 1", Some(10)));
    let seen = run_done(&ev, 2).await;
    assert!(matches!(seen[0], Event::Connected { .. }), "{seen:?}");
    assert!(has(&seen, |e| matches!(
        e,
        Event::StatementDone {
            end: StatementEnd::Complete(_),
            ..
        }
    )));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_panic_closes_the_session_it_held() {
    let fx = fixture("", "");
    let conn = fx.ws.connection("lite").unwrap();
    let session = dbento_core::connect(&fx.ws, conn, None, Default::default())
        .await
        .unwrap();
    let slot: SessionSlot = Arc::new(tokio::sync::Mutex::new(Some(session)));
    let (tx, rx) = async_channel::bounded(4);
    let held = slot.clone();
    guarded(
        async move {
            let _guard = held.lock().await;
            panic!("boom")
        },
        slot.clone(),
        tx,
    )
    .await;
    assert!(
        slot.lock().await.is_none(),
        "the session was taken and closed"
    );
    assert!(matches!(rx.recv().await.unwrap(), Event::Stopped { .. }));
}

// The UPDATE's prepare already settled the truncated SELECT, so nothing is left to drain.
#[tokio::test(flavor = "multi_thread")]
async fn a_skipped_last_statement_after_a_truncated_one_does_not_settle() {
    let fx = fixture("CREATE TABLE t(x);", "");
    let r = spawn_tuned(&here(), spec(&fx, "lite"), tuning());
    let ev = r.events();
    r.send(run_cmd(
        1,
        &format!("{MANY}; UPDATE t SET x = 0"),
        Some(1000),
    ));
    until_event(&ev, |e| {
        matches!(
            e,
            Event::NeedsConfirm {
                run_id: 1,
                stmt: 1,
                ..
            }
        )
    })
    .await;
    r.send(Command::Skip { run_id: 1, stmt: 1 });
    let seen = run_done(&ev, 1).await;
    assert!(
        has(&seen, |e| matches!(
            e,
            Event::StatementDone {
                stmt: 1,
                end: StatementEnd::Skipped,
                ..
            }
        )),
        "{seen:?}"
    );
    assert!(
        !has(&seen, |e| matches!(e, Event::Settling { .. })),
        "{seen:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_truncated_write_whose_commit_fails_is_reported() {
    let (db, _dir, spec) = pg_spec("").await;
    db.admin()
        .await
        .batch_execute(
            "CREATE TABLE p(a int PRIMARY KEY);
             CREATE TABLE c(a int REFERENCES p DEFERRABLE INITIALLY DEFERRED)",
        )
        .await
        .unwrap();
    let r = spawn_tuned(&here(), spec, tuning());
    let ev = r.events();
    // No parent row exists, so the commit after the cut-short result fails its deferred check.
    r.send(run_cmd(
        1,
        "INSERT INTO c SELECT generate_series(1, 100) RETURNING a",
        Some(10),
    ));
    let seen = run_done(&ev, 1).await;
    assert!(
        has(
            &seen,
            |e| matches!(e, Event::SettleFailed { message } if message.contains("23503"))
        ),
        "{seen:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_postgres_stop_at_the_write_confirmation_skips_it_and_keeps_the_session() {
    let (_db, _dir, spec) = pg_spec("").await;
    let r = spawn_tuned(&here(), spec, tuning());
    let ev = r.events();
    let script = "CREATE TEMP TABLE t AS SELECT 1 AS x; UPDATE t SET x = 0; SELECT 5";
    r.send(run_cmd(1, script, Some(10)));
    until_event(&ev, |e| matches!(e, Event::NeedsConfirm { stmt: 1, .. })).await;
    r.send(Command::Stop { run_id: 1 });
    let seen = run_done(&ev, 1).await;
    assert!(
        has(&seen, |e| matches!(
            e,
            Event::StatementDone {
                stmt: 1,
                end: StatementEnd::Skipped,
                ..
            }
        )),
        "{seen:?}"
    );
    assert!(!has(&seen, |e| matches!(
        e,
        Event::Executing { stmt: 2, .. } | Event::Disconnected { .. } | Event::SettleFailed { .. }
    )));
    // The temp table lives only in this session, so finding it unchanged proves both the
    // session was kept and the UPDATE never ran.
    r.send(run_cmd(2, "SELECT x FROM t WHERE x = 1", Some(10)));
    let (seen, rows) = until_event(&ev, |e| matches!(e, Event::RunDone { run_id: 2 })).await;
    assert!(
        !has(&seen, |e| matches!(e, Event::Connected { .. })),
        "{seen:?}"
    );
    assert_eq!(rows, 1, "{seen:?}");
}
