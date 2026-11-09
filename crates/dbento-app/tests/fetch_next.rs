//! The runner parks the last read at its row cap and continues it on `Fetch`.
use crate::support::{MANY, fixture, here, next_event, pg_spec, quiet, run_cmd, spec, until_event};
use dbento_app::runner::{
    AppLimits, Command, Event, RunnerTuning, StatementEnd, StopReason, spawn_tuned,
};
use dbento_core::ExecuteError;
use std::time::Duration;

const ENDLESS: &str =
    "WITH RECURSIVE r(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM r) SELECT x FROM r";

fn tuned(park_ms: u64) -> RunnerTuning {
    RunnerTuning {
        park_at_cap: Duration::from_millis(park_ms),
        ..RunnerTuning::default()
    }
}
fn parked_at(e: &Event, id: u64) -> bool {
    matches!(e, Event::ParkedAtCap { run_id, .. } if *run_id == id)
}
fn run_done(e: &Event, id: u64) -> bool {
    matches!(e, Event::RunDone { run_id } if *run_id == id)
}
fn end_of(seen: &[Event], id: u64) -> Option<&StatementEnd> {
    seen.iter().find_map(|e| match e {
        Event::StatementDone { run_id, end, .. } if *run_id == id => Some(end),
        _ => None,
    })
}
fn truncated_at(seen: &[Event], id: u64, rows: u64) -> bool {
    matches!(end_of(seen, id), Some(StatementEnd::Truncated(o)) if o.rows == rows && o.truncated)
}

#[tokio::test(flavor = "multi_thread")]
async fn the_last_read_parks_at_its_cap_and_ignores_pull() {
    let fx = fixture("", "");
    let r = spawn_tuned(&here(), spec(&fx, "lite"), tuned(30_000));
    let ev = r.events();
    r.send(run_cmd(1, MANY, Some(1000)));
    let (seen, rows) = until_event(&ev, |e| parked_at(e, 1)).await;
    assert_eq!(rows, 1000);
    match seen.last() {
        Some(Event::ParkedAtCap {
            stmt: 0,
            outcome,
            elapsed,
            ..
        }) => {
            assert_eq!((outcome.rows, outcome.truncated), (1000, true));
            assert!(*elapsed < Duration::from_secs(5), "{elapsed:?}");
        }
        other => panic!("{other:?}"),
    }
    assert!(
        !seen
            .iter()
            .any(|e| matches!(e, Event::StatementDone { .. } | Event::RunDone { .. }))
    );
    r.send(Command::Pull { run_id: 1 });
    r.send(Command::Fetch {
        run_id: 0,
        more: None,
    }); // stale
    assert!(
        quiet(&ev).await,
        "neither a Pull nor a stale Fetch passes the cap"
    );
    r.send(Command::Fetch {
        run_id: 1,
        more: None,
    });
    assert!(
        matches!(next_event(&ev).await, Event::Batch { run_id: 1, .. }),
        "the parked stream is still live"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn fetch_next_reparks_with_a_fresh_timer_and_fetch_all_completes() {
    let fx = fixture("", "");
    let r = spawn_tuned(&here(), spec(&fx, "lite"), tuned(1500));
    let ev = r.events();
    r.send(run_cmd(1, MANY, Some(1000)));
    until_event(&ev, |e| parked_at(e, 1)).await;
    tokio::time::sleep(Duration::from_millis(1000)).await;
    r.send(Command::Fetch {
        run_id: 1,
        more: Some(1000),
    });
    let (seen, rows) = until_event(&ev, |e| parked_at(e, 1)).await;
    assert_eq!(rows, 1000);
    assert!(
        matches!(seen.last(), Some(Event::ParkedAtCap { outcome, .. }) if outcome.rows == 2000)
    );
    // The first park's timer would have fired 1.5 s after it began; a fresh one has not.
    tokio::time::sleep(Duration::from_millis(1000)).await;
    assert!(ev.is_empty(), "the park expired early");
    r.send(Command::Fetch {
        run_id: 1,
        more: None,
    });
    let (seen, rows) = until_event(&ev, |e| run_done(e, 1)).await;
    assert_eq!(rows, 3000);
    assert!(
        matches!(end_of(&seen, 1), Some(StatementEnd::Complete(o)) if o.rows == 5000 && !o.truncated),
        "{seen:?}"
    );
    assert!(
        !seen
            .iter()
            .any(|e| matches!(e, Event::ParkExpired { .. } | Event::Settling { .. }))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn park_expiry_settles_a_read_write_session() {
    let fx = fixture("", "");
    let r = spawn_tuned(&here(), spec(&fx, "lite"), tuned(300));
    let ev = r.events();
    r.send(run_cmd(1, MANY, Some(1000)));
    let (seen, _) = until_event(&ev, |e| run_done(e, 1)).await;
    let expired = seen.iter().position(|e| {
        matches!(
            e,
            Event::ParkExpired {
                run_id: 1,
                statement_timeout: false
            }
        )
    });
    let ended = seen.iter().position(|e| matches!(e, Event::StatementDone { run_id: 1, end: StatementEnd::Truncated(o), .. } if o.rows == 1000));
    assert!(
        matches!((expired, ended), (Some(a), Some(b)) if a < b),
        "{seen:?}"
    );
    assert!(
        seen.iter()
            .any(|e| matches!(e, Event::Settling { run_id: 1 })),
        "{seen:?}"
    );
    assert!(!seen.iter().any(|e| matches!(e, Event::Disconnected { .. })));
    r.send(run_cmd(2, "SELECT 1", Some(1000)));
    assert!(
        matches!(next_event(&ev).await, Event::Executing { run_id: 2, .. }),
        "no reconnect"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn park_expiry_closes_a_read_only_session() {
    let fx = fixture("", "read_only = true");
    let r = spawn_tuned(&here(), spec(&fx, "lite"), tuned(300));
    let ev = r.events();
    r.send(run_cmd(1, MANY, Some(1000)));
    let (seen, _) = until_event(&ev, |e| run_done(e, 1)).await;
    assert!(truncated_at(&seen, 1, 1000), "{seen:?}");
    assert!(
        seen.iter().any(|e| matches!(e, Event::Disconnected { .. })),
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
async fn stop_at_the_cap_ends_truncated_not_stopped() {
    let fx = fixture("", "");
    let r = spawn_tuned(&here(), spec(&fx, "lite"), tuned(30_000));
    let ev = r.events();
    r.send(run_cmd(1, MANY, Some(1000)));
    until_event(&ev, |e| parked_at(e, 1)).await;
    r.send(Command::Stop { run_id: 1 });
    let (seen, _) = until_event(&ev, |e| run_done(e, 1)).await;
    assert!(truncated_at(&seen, 1, 1000), "{seen:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_run_at_the_cap_ends_the_old_statement_truncated() {
    let fx = fixture("", "");
    let r = spawn_tuned(&here(), spec(&fx, "lite"), tuned(30_000));
    let ev = r.events();
    r.send(run_cmd(1, MANY, Some(1000)));
    until_event(&ev, |e| parked_at(e, 1)).await;
    r.send(run_cmd(2, "SELECT 1", Some(1000)));
    let (seen, _) = until_event(&ev, |e| run_done(e, 2)).await;
    assert!(truncated_at(&seen, 1, 1000), "{seen:?}");
    assert!(matches!(end_of(&seen, 2), Some(StatementEnd::Complete(o)) if o.rows == 1));
}

#[tokio::test(flavor = "multi_thread")]
async fn close_at_the_cap_ends_truncated_and_ends_the_runner() {
    let fx = fixture("", "");
    let r = spawn_tuned(&here(), spec(&fx, "lite"), tuned(30_000));
    let ev = r.events();
    r.send(run_cmd(1, MANY, Some(1000)));
    until_event(&ev, |e| parked_at(e, 1)).await;
    r.send(Command::Close);
    let mut seen = Vec::new();
    let ended = tokio::time::timeout(Duration::from_secs(10), async {
        while let Ok(e) = ev.recv().await {
            seen.push(e);
        }
    })
    .await;
    assert!(ended.is_ok(), "the runner did not end");
    assert!(truncated_at(&seen, 1, 1000), "{seen:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_write_does_not_park() {
    let fx = fixture("CREATE TABLE t(x);", "");
    let r = spawn_tuned(&here(), spec(&fx, "lite"), tuned(30_000));
    let ev = r.events();
    let sql = "WITH RECURSIVE r(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM r WHERE x < 50) INSERT INTO t SELECT x FROM r RETURNING x";
    r.send(run_cmd(1, sql, Some(10)));
    // A park would hold the run past next_event's 20 s limit.
    let (seen, rows) = until_event(&ev, |e| run_done(e, 1)).await;
    assert_eq!(rows, 10);
    assert!(!seen.iter().any(|e| matches!(e, Event::ParkedAtCap { .. })));
    assert!(truncated_at(&seen, 1, 10), "{seen:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn fetch_is_ignored_below_the_cap() {
    let fx = fixture("", "");
    let r = spawn_tuned(&here(), spec(&fx, "lite"), tuned(30_000));
    let ev = r.events();
    r.send(run_cmd(1, MANY, Some(10_000)));
    until_event(&ev, |e| matches!(e, Event::Paused { run_id: 1, .. })).await;
    r.send(Command::Fetch {
        run_id: 1,
        more: None,
    });
    assert!(quiet(&ev).await, "a Fetch below the cap does nothing");
    r.send(Command::Pull { run_id: 1 });
    assert!(
        matches!(next_event(&ev).await, Event::Batch { run_id: 1, batch, .. } if batch.len() == 1024)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_memory_budget_still_ends_a_fetch_all() {
    let fx = fixture("", "");
    let r = spawn_tuned(&here(), spec(&fx, "lite"), tuned(30_000));
    let ev = r.events();
    r.send(Command::Run {
        run_id: 1,
        sql: ENDLESS.into(),
        limits: AppLimits {
            max_rows: Some(1000),
            max_result_bytes: 1 << 20,
        },
    });
    until_event(&ev, |e| parked_at(e, 1)).await;
    r.send(Command::Fetch {
        run_id: 1,
        more: None,
    });
    let (seen, _) = until_event(&ev, |e| run_done(e, 1)).await;
    assert!(
        matches!(
            end_of(&seen, 1),
            Some(StatementEnd::Stopped {
                reason: StopReason::MemoryBudget,
                ..
            })
        ),
        "{seen:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_time_parked_at_the_cap_is_not_statement_time() {
    let fx = fixture("", "");
    let r = spawn_tuned(&here(), spec(&fx, "lite"), tuned(30_000));
    let ev = r.events();
    r.send(run_cmd(1, MANY, Some(1000)));
    until_event(&ev, |e| parked_at(e, 1)).await;
    tokio::time::sleep(Duration::from_millis(1000)).await;
    r.send(Command::Stop { run_id: 1 });
    let (seen, _) = until_event(&ev, |e| run_done(e, 1)).await;
    let elapsed = seen
        .iter()
        .find_map(|e| match e {
            Event::StatementDone { elapsed, .. } => Some(*elapsed),
            _ => None,
        })
        .unwrap();
    assert!(elapsed < Duration::from_millis(900), "{elapsed:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_statement_timeout_after_the_park_reads_as_expiry() {
    let fx = fixture("", "statement_timeout = \"1s\"");
    let r = spawn_tuned(&here(), spec(&fx, "lite"), tuned(30_000));
    let ev = r.events();
    r.send(run_cmd(1, MANY, Some(1000)));
    until_event(&ev, |e| parked_at(e, 1)).await;
    // SQLite's deadline is fixed at execute, so stepping again past it times out.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    r.send(Command::Fetch {
        run_id: 1,
        more: Some(1000),
    });
    let (seen, _) = until_event(&ev, |e| run_done(e, 1)).await;
    let expired = seen.iter().position(|e| {
        matches!(
            e,
            Event::ParkExpired {
                run_id: 1,
                statement_timeout: true
            }
        )
    });
    let ended = seen.iter().position(|e| matches!(e, Event::StatementDone { end: StatementEnd::Truncated(o), .. } if o.rows == 1000));
    assert!(
        matches!((expired, ended), (Some(a), Some(b)) if a < b),
        "{seen:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn cancel_during_a_fetch_ends_truncated_with_the_rows() {
    let fx = fixture("", "");
    let r = spawn_tuned(&here(), spec(&fx, "lite"), tuned(30_000));
    let ev = r.events();
    r.send(run_cmd(1, ENDLESS, Some(1000)));
    let Event::Connected { cancel, .. } = next_event(&ev).await else {
        panic!("not connected")
    };
    until_event(&ev, |e| parked_at(e, 1)).await;
    r.send(Command::Fetch {
        run_id: 1,
        more: None,
    });
    until_event(&ev, |e| matches!(e, Event::Batch { run_id: 1, .. })).await;
    cancel.cancel();
    let (seen, _) = until_event(&ev, |e| run_done(e, 1)).await;
    assert!(
        matches!(end_of(&seen, 1), Some(StatementEnd::Truncated(o)) if o.rows > 1000),
        "{seen:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_connection_lost_after_the_park_stops_as_lost_and_disconnects() {
    let (db, _dir, spec) = pg_spec("").await;
    let r = spawn_tuned(&here(), spec, tuned(30_000));
    let ev = r.events();
    r.send(run_cmd(
        1,
        "SELECT g FROM generate_series(1, 2000000) g",
        Some(1000),
    ));
    until_event(&ev, |e| parked_at(e, 1)).await;
    db.scalar("SELECT count(pg_terminate_backend(pid)) FROM pg_stat_activity WHERE application_name = 'dbento' AND datname = current_database()").await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    // Fetch all reads past whatever the socket still buffers, so the loss surfaces.
    r.send(Command::Fetch {
        run_id: 1,
        more: None,
    });
    let (seen, _) = until_event(&ev, |e| run_done(e, 1)).await;
    assert!(
        matches!(
            end_of(&seen, 1),
            Some(StatementEnd::Stopped { reason: StopReason::Lost, rows }) if *rows >= 1000
        ),
        "{seen:?}"
    );
    assert!(
        seen.iter().any(|e| matches!(e, Event::Disconnected { .. })),
        "{seen:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn another_error_after_the_park_fails_as_today() {
    let (_db, _dir, spec) = pg_spec("").await;
    let r = spawn_tuned(&here(), spec, tuned(30_000));
    let ev = r.events();
    r.send(run_cmd(
        1,
        "SELECT 1 / (1500 - g) FROM generate_series(1, 5000) g",
        Some(1000),
    ));
    until_event(&ev, |e| parked_at(e, 1)).await;
    r.send(Command::Fetch {
        run_id: 1,
        more: None,
    });
    let (seen, _) = until_event(&ev, |e| run_done(e, 1)).await;
    assert!(
        matches!(
            end_of(&seen, 1),
            Some(StatementEnd::Failed(ExecuteError::Database { code, .. })) if code == "22012"
        ),
        "{seen:?}"
    );
}
