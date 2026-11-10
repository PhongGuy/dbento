//! The runner parks the last statement's stream and pulls it one batch per `Pull`.
use crate::support::{MANY, fixture, here, next_event, quiet, run_cmd, spec, until_event};
use dbento_app::results::ResultStore;
use dbento_app::results_pane::summary;
use dbento_app::runner::{Command, Event, RunnerTuning, StatementEnd, StopReason, spawn_tuned};
use dbento_app::tab_model::StatementResult;
use std::sync::Arc;
use std::time::Duration;

fn tuned(idle_ms: u64) -> RunnerTuning {
    RunnerTuning {
        first_rows: 200,
        idle_complete: Duration::from_millis(idle_ms),
        events_capacity: 64,
        park_at_cap: Duration::from_millis(idle_ms),
    }
}

// SQLite batches close at 1,024 rows (core `MAX_BATCH_ROWS`), so a screenful plus one batch is 2,048.
#[tokio::test(flavor = "multi_thread")]
async fn the_last_statement_parks_and_each_pull_sends_one_batch() {
    let fx = fixture("", "");
    let r = spawn_tuned(&here(), spec(&fx, "lite"), tuned(60_000));
    let ev = r.events();
    r.send(run_cmd(1, MANY, Some(10_000)));
    let (_, rows) = until_event(&ev, |e| matches!(e, Event::Paused { run_id: 1, stmt: 0 })).await;
    assert_eq!(rows, 2048);
    assert!(quiet(&ev).await, "nothing more without a Pull");
    r.send(Command::Pull { run_id: 0 }); // stale
    assert!(quiet(&ev).await);
    for want in [1024, 1024] {
        r.send(Command::Pull { run_id: 1 });
        assert!(
            matches!(next_event(&ev).await, Event::Batch { run_id: 1, batch, .. } if batch.len() == want)
        );
        assert!(quiet(&ev).await, "one batch per Pull, and Paused only once");
    }
    // The read-ahead behind the last batch finds the end, so no further Pull is needed.
    r.send(Command::Pull { run_id: 1 });
    assert!(
        matches!(next_event(&ev).await, Event::Batch { run_id: 1, batch, .. } if batch.len() == 904)
    );
    assert!(
        matches!(next_event(&ev).await, Event::StatementDone { end: StatementEnd::Complete(o), .. } if o.rows == 5000)
    );
    assert!(matches!(
        next_event(&ev).await,
        Event::RunDone { run_id: 1 }
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_result_that_ends_where_it_would_park_completes_without_a_pull() {
    let fx = fixture("", "");
    let r = spawn_tuned(&here(), spec(&fx, "lite"), tuned(60_000));
    let ev = r.events();
    let sql = "WITH RECURSIVE r(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM r WHERE x < 1500) SELECT x FROM r";
    r.send(run_cmd(1, sql, Some(10_000)));
    let wait = until_event(&ev, |e| matches!(e, Event::RunDone { run_id: 1 }));
    let (seen, rows) = tokio::time::timeout(Duration::from_secs(5), wait)
        .await
        .expect("the run ends without a Pull");
    assert_eq!(rows, 1500);
    assert!(!seen.iter().any(|e| matches!(e, Event::Paused { .. })));
    assert!(seen.iter().any(
        |e| matches!(e, Event::StatementDone { end: StatementEnd::Complete(o), .. } if o.rows == 1500)
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn idle_complete_pulls_the_rest_up_to_the_cap() {
    let fx = fixture("", "");
    let r = spawn_tuned(&here(), spec(&fx, "lite"), tuned(300));
    let ev = r.events();
    r.send(run_cmd(1, MANY, Some(10_000)));
    let (seen, rows) = until_event(&ev, |e| matches!(e, Event::RunDone { run_id: 1 })).await;
    assert_eq!(rows, 5000);
    assert!(seen.iter().any(|e| matches!(e, Event::Paused { .. })));
    assert!(seen.iter().any(|e| matches!(
        e,
        Event::StatementDone {
            end: StatementEnd::Complete(_),
            ..
        }
    )));
    r.send(run_cmd(2, MANY, Some(3000)));
    let (seen, rows) = until_event(&ev, |e| matches!(e, Event::RunDone { run_id: 2 })).await;
    assert_eq!(rows, 3000);
    assert!(seen.iter().any(
        |e| matches!(e, Event::StatementDone { end: StatementEnd::Truncated(o), .. } if o.rows == 3000)
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn idle_complete_counts_from_the_last_pull() {
    let fx = fixture("", "");
    let r = spawn_tuned(&here(), spec(&fx, "lite"), tuned(2000));
    let ev = r.events();
    r.send(run_cmd(1, MANY, Some(10_000)));
    until_event(&ev, |e| matches!(e, Event::Paused { .. })).await;
    tokio::time::sleep(Duration::from_millis(1200)).await;
    r.send(Command::Pull { run_id: 1 });
    assert!(matches!(next_event(&ev).await, Event::Batch { .. }));
    let pulled = std::time::Instant::now();
    let (_, rows) = until_event(&ev, |e| matches!(e, Event::RunDone { run_id: 1 })).await;
    // Counting from the first park would end the run about 800 ms after the Pull.
    assert!(
        pulled.elapsed() >= Duration::from_millis(1500),
        "the timer did not restart: {:?}",
        pulled.elapsed()
    );
    assert_eq!(rows, 5000 - 3072);
}

#[tokio::test(flavor = "multi_thread")]
async fn earlier_statements_run_to_the_end_without_pulls() {
    let fx = fixture("", "");
    let r = spawn_tuned(&here(), spec(&fx, "lite"), tuned(60_000));
    let ev = r.events();
    r.send(run_cmd(1, &format!("{MANY}; SELECT 1"), Some(10_000)));
    let (seen, rows) = until_event(&ev, |e| matches!(e, Event::RunDone { run_id: 1 })).await;
    assert_eq!(rows, 5001);
    assert!(!seen.iter().any(|e| matches!(e, Event::Paused { .. })));
    r.send(run_cmd(2, &format!("SELECT 1; {MANY}"), Some(10_000)));
    until_event(&ev, |e| matches!(e, Event::Paused { run_id: 2, stmt: 1 })).await;
}

fn bounded(capacity: usize) -> RunnerTuning {
    RunnerTuning {
        events_capacity: capacity,
        ..tuned(60_000)
    }
}
fn stopped(seen: &[Event], stmt: usize, reason: StopReason) -> bool {
    seen.iter().any(|e| matches!(e, Event::StatementDone { stmt: s, end: StatementEnd::Stopped { reason: r, .. }, .. } if *s == stmt && *r == reason))
}

#[tokio::test(flavor = "multi_thread")]
async fn stop_ends_a_parked_stream_and_keeps_its_rows() {
    let fx = fixture("", "");
    let r = spawn_tuned(&here(), spec(&fx, "lite"), tuned(60_000));
    let ev = r.events();
    r.send(run_cmd(1, MANY, Some(10_000)));
    until_event(&ev, |e| matches!(e, Event::Paused { .. })).await;
    r.send(Command::Stop { run_id: 0 }); // stale
    assert!(quiet(&ev).await);
    // Still parked: a Pull gets exactly one batch.
    r.send(Command::Pull { run_id: 1 });
    assert!(
        matches!(next_event(&ev).await, Event::Batch { run_id: 1, batch, .. } if batch.len() == 1024)
    );
    r.send(Command::Stop { run_id: 1 });
    let (seen, _) = until_event(&ev, |e| matches!(e, Event::RunDone { run_id: 1 })).await;
    assert!(
        seen.iter().any(|e| matches!(
            e,
            Event::StatementDone {
                end: StatementEnd::Stopped {
                    rows: 3072,
                    reason: StopReason::User
                },
                ..
            }
        )),
        "{seen:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_run_stops_a_parked_stream() {
    let fx = fixture("", "");
    let r = spawn_tuned(&here(), spec(&fx, "lite"), tuned(60_000));
    let ev = r.events();
    r.send(run_cmd(1, MANY, Some(10_000)));
    until_event(&ev, |e| matches!(e, Event::Paused { .. })).await;
    r.send(run_cmd(2, "SELECT 1", Some(10_000)));
    let (seen, _) = until_event(&ev, |e| matches!(e, Event::RunDone { run_id: 1 })).await;
    assert!(stopped(&seen, 0, StopReason::NewRun), "{seen:?}");
    let (seen, _) = until_event(&ev, |e| matches!(e, Event::RunDone { run_id: 2 })).await;
    assert!(seen.iter().any(|e| matches!(e, Event::StatementDone { run_id: 2, end: StatementEnd::Complete(o), .. } if o.rows == 1)));
}

#[tokio::test(flavor = "multi_thread")]
async fn stop_during_an_earlier_statement_ends_the_whole_script() {
    let fx = fixture("", "");
    let r = spawn_tuned(&here(), spec(&fx, "lite"), bounded(2));
    let ev = r.events();
    r.send(run_cmd(
        1,
        &format!("{MANY}; {MANY}; SELECT 1"),
        Some(10_000),
    ));
    until_event(&ev, |e| matches!(e, Event::Batch { .. })).await;
    r.send(Command::Stop { run_id: 1 });
    let (seen, _) = until_event(&ev, |e| matches!(e, Event::RunDone { run_id: 1 })).await;
    assert!(stopped(&seen, 0, StopReason::User), "{seen:?}");
    assert!(!seen.iter().any(|e| matches!(
        e,
        Event::Executing { stmt: 1, .. } | Event::StatementStarted { stmt: 1, .. }
    )));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_event_channel_is_bounded() {
    let fx = fixture("", "");
    let r = spawn_tuned(&here(), spec(&fx, "lite"), bounded(2));
    let ev = r.events();
    r.send(run_cmd(1, &format!("{MANY}; SELECT 1"), Some(10_000)));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(ev.len() <= 2, "{} queued", ev.len());
    let (_, rows) = until_event(&ev, |e| matches!(e, Event::RunDone { run_id: 1 })).await;
    assert_eq!(rows, 5001);
}

#[tokio::test(flavor = "multi_thread")]
async fn close_while_parked_ends_the_runner() {
    let fx = fixture("", "");
    let r = spawn_tuned(&here(), spec(&fx, "lite"), tuned(60_000));
    let ev = r.events();
    r.send(run_cmd(1, MANY, Some(10_000)));
    until_event(&ev, |e| matches!(e, Event::Paused { .. })).await;
    r.send(Command::Close);
    let ended = tokio::time::timeout(Duration::from_secs(10), async {
        while ev.recv().await.is_ok() {}
    })
    .await;
    assert!(ended.is_ok(), "the runner did not end");
}

#[test]
fn a_stopped_statement_says_so() {
    let summary_of = |end| {
        let mut store = ResultStore::new(Arc::new([]));
        store.finish(end);
        summary(&StatementResult {
            stmt: 0,
            sql: String::new(),
            engine: None,
            epoch: 0,
            store,
            elapsed: None,
            source: None,
            plan: None,
        })
    };
    assert_eq!(
        summary_of(StatementEnd::Stopped {
            rows: 2048,
            reason: StopReason::User
        }),
        "2,048 rows (stopped)"
    );
    assert_eq!(
        summary_of(StatementEnd::Stopped {
            rows: 3,
            reason: StopReason::Lost
        }),
        "session stopped"
    );
}

/// Waits until the runner has filled the event channel and is blocked sending one more.
async fn full(ev: &async_channel::Receiver<Event>) {
    while !ev.is_full() {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    // The runner stays blocked in its send; the pause only lets it get there.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(ev.is_full(), "the runner stays blocked on the full channel");
}

#[tokio::test(flavor = "multi_thread")]
async fn stop_takes_effect_while_the_event_channel_is_full() {
    let fx = fixture("", "");
    let r = spawn_tuned(&here(), spec(&fx, "lite"), bounded(4));
    let ev = r.events();
    r.send(run_cmd(
        1,
        &format!("{MANY}; {MANY}; SELECT 1"),
        Some(10_000),
    ));
    full(&ev).await;
    r.send(Command::Stop { run_id: 1 });
    let (seen, rows) = until_event(&ev, |e| matches!(e, Event::RunDone { run_id: 1 })).await;
    assert!(stopped(&seen, 0, StopReason::User), "{seen:?}");
    assert!(rows < 5000, "{rows} rows streamed after Stop");
    assert!(
        !seen
            .iter()
            .any(|e| matches!(e, Event::Executing { stmt: 1, .. }))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn close_takes_effect_while_the_event_channel_is_full() {
    let fx = fixture("", "");
    let r = spawn_tuned(&here(), spec(&fx, "lite"), bounded(4));
    let ev = r.events();
    r.send(run_cmd(
        1,
        &format!("{MANY}; {MANY}; SELECT 1"),
        Some(10_000),
    ));
    full(&ev).await;
    r.send(Command::Close);
    let (seen, _) = until_event(&ev, |e| matches!(e, Event::RunDone { run_id: 1 })).await;
    assert!(stopped(&seen, 0, StopReason::Closed), "{seen:?}");
    let ended = tokio::time::timeout(Duration::from_secs(10), async {
        while ev.recv().await.is_ok() {}
    })
    .await;
    assert!(ended.is_ok(), "the runner did not end");
}

// Events run Connected, Executing, StatementStarted, Batch, Batch: with room for four, the
// runner blocks sending the batch that spends its credit, and a Pull sent then must count.
#[tokio::test(flavor = "multi_thread")]
async fn a_pull_sent_just_before_the_park_releases_one_batch() {
    let fx = fixture("", "");
    let r = spawn_tuned(&here(), spec(&fx, "lite"), bounded(4));
    let ev = r.events();
    r.send(run_cmd(1, MANY, Some(10_000)));
    full(&ev).await;
    r.send(Command::Pull { run_id: 1 });
    let (seen, rows) = until_event(&ev, |e| matches!(e, Event::Paused { .. })).await;
    assert_eq!(rows, 3072, "{seen:?}");
    assert!(quiet(&ev).await, "one batch for the one Pull");
    // An extra unsolicited batch would shift these: 1024, then the 904-row tail and the end.
    r.send(Command::Pull { run_id: 1 });
    assert!(matches!(next_event(&ev).await, Event::Batch { batch, .. } if batch.len() == 1024));
    r.send(Command::Pull { run_id: 1 });
    assert!(matches!(next_event(&ev).await, Event::Batch { batch, .. } if batch.len() == 904));
    assert!(
        matches!(next_event(&ev).await, Event::StatementDone { end: StatementEnd::Complete(o), .. } if o.rows == 5000)
    );
}

// With room for two commands, the Stop behind eight stale Pulls was dropped by `try_send`.
#[tokio::test(flavor = "multi_thread")]
async fn commands_sent_while_the_runner_is_blocked_are_never_dropped() {
    let fx = fixture("", "");
    let r = spawn_tuned(&here(), spec(&fx, "lite"), bounded(2));
    let ev = r.events();
    r.send(run_cmd(
        1,
        &format!("{MANY}; {MANY}; SELECT 1"),
        Some(10_000),
    ));
    full(&ev).await;
    for _ in 0..8 {
        r.send(Command::Pull { run_id: 0 });
    }
    r.send(Command::Stop { run_id: 1 });
    let (seen, _) = until_event(&ev, |e| matches!(e, Event::RunDone { run_id: 1 })).await;
    assert!(stopped(&seen, 0, StopReason::User), "{seen:?}");
    assert!(
        !seen
            .iter()
            .any(|e| matches!(e, Event::Executing { stmt: 1, .. }))
    );
}

fn done_elapsed(seen: &[Event]) -> Duration {
    seen.iter()
        .find_map(|e| match e {
            Event::StatementDone { elapsed, .. } => Some(*elapsed),
            _ => None,
        })
        .unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn elapsed_leaves_out_the_wait_for_pulls() {
    let fx = fixture("", "");
    let r = spawn_tuned(&here(), spec(&fx, "lite"), tuned(60_000));
    let ev = r.events();
    r.send(run_cmd(1, MANY, Some(10_000)));
    until_event(&ev, |e| matches!(e, Event::Paused { run_id: 1, .. })).await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    let mut seen = Vec::new();
    while !seen.iter().any(|e| matches!(e, Event::RunDone { .. })) {
        r.send(Command::Pull { run_id: 1 });
        seen.push(next_event(&ev).await);
    }
    assert!(
        done_elapsed(&seen) < Duration::from_millis(800),
        "{:?} counts the park",
        done_elapsed(&seen)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn elapsed_of_a_stopped_park_leaves_out_the_wait() {
    let fx = fixture("", "");
    let r = spawn_tuned(&here(), spec(&fx, "lite"), tuned(60_000));
    let ev = r.events();
    r.send(run_cmd(1, MANY, Some(10_000)));
    until_event(&ev, |e| matches!(e, Event::Paused { run_id: 1, .. })).await;
    tokio::time::sleep(Duration::from_millis(800)).await;
    r.send(Command::Stop { run_id: 1 });
    let (seen, _) = until_event(&ev, |e| matches!(e, Event::RunDone { run_id: 1 })).await;
    assert!(done_elapsed(&seen) < Duration::from_millis(800));
}
