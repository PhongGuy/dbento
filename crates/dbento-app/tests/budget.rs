//! A result stops at the memory budget, and the default budget fits a million wide rows.
use crate::support;
use crate::support::{MANY, fixture, here, label_of, opened, spec, until_event};
use dbento_app::results::ResultStore;
use dbento_app::results_pane::summary;
use dbento_app::runner::{
    AppLimits, Command, DEFAULT_MAX_RESULT_BYTES, Event, RunnerTuning, StatementEnd, StopReason,
    WIDE_1M_HEAP_BYTES, spawn_tuned,
};
use dbento_app::tab_model::StatementResult;
use gpui_kit::TestAppContext;
use std::time::Duration;

fn eager() -> RunnerTuning {
    RunnerTuning {
        first_rows: u64::MAX,
        idle_complete: Duration::from_secs(60),
        events_capacity: 64,
        park_at_cap: Duration::from_secs(60),
    }
}
fn run(run_id: u64, sql: String, budget: u64) -> Command {
    Command::Run {
        run_id,
        sql,
        limits: AppLimits {
            max_rows: None,
            max_result_bytes: budget,
        },
    }
}
#[tokio::test(flavor = "multi_thread")]
async fn the_batch_that_would_pass_the_budget_is_not_sent_and_the_run_ends() {
    // `batches` runs its own runtime, so it cannot block this one's thread.
    let (_, core) = tokio::task::spawn_blocking(|| support::batches(MANY))
        .await
        .unwrap();
    let budget = (core[0].heap_bytes() + core[1].heap_bytes()) as u64; // exactly two batches
    let fx = fixture("", "");
    let r = spawn_tuned(&here(), spec(&fx, "lite"), eager());
    let ev = r.events();
    r.send(run(1, format!("{MANY}; SELECT 'after'"), budget));
    let (seen, rows) = until_event(&ev, |e| matches!(e, Event::RunDone { run_id: 1 })).await;
    assert_eq!(rows, 2048);
    assert!(
        seen.iter().any(|e| matches!(
            e,
            Event::StatementDone {
                stmt: 0,
                end: StatementEnd::Stopped {
                    rows: 2048,
                    reason: StopReason::MemoryBudget
                },
                ..
            }
        )),
        "{seen:?}"
    );
    assert!(
        !seen
            .iter()
            .any(|e| matches!(e, Event::Executing { stmt: 1, .. })),
        "the run ends"
    );
}
#[tokio::test(flavor = "multi_thread")]
async fn a_result_within_the_budget_completes() {
    // `batches` runs its own runtime, so it cannot block this one's thread.
    let (_, core) = tokio::task::spawn_blocking(|| support::batches(MANY))
        .await
        .unwrap();
    let budget = core.iter().map(|b| b.heap_bytes() as u64).sum();
    let fx = fixture("", "");
    let r = spawn_tuned(&here(), spec(&fx, "lite"), eager());
    let ev = r.events();
    r.send(run(1, MANY.into(), budget));
    let (seen, rows) = until_event(&ev, |e| matches!(e, Event::RunDone { run_id: 1 })).await;
    assert_eq!(rows, 5000);
    assert!(seen.iter().any(|e| matches!(
        e,
        Event::StatementDone {
            end: StatementEnd::Complete(_),
            ..
        }
    )));
}
#[test]
fn the_default_budget_covers_a_million_wide_rows_with_margin() {
    const { assert!(DEFAULT_MAX_RESULT_BYTES >= WIDE_1M_HEAP_BYTES * 5 / 4) };
    assert_eq!(DEFAULT_MAX_RESULT_BYTES % (64 << 20), 0);
    assert_eq!(
        AppLimits::default().max_result_bytes,
        DEFAULT_MAX_RESULT_BYTES
    );
    assert_eq!(AppLimits::default().max_rows, Some(1_000));
}
#[test]
fn a_budget_stop_reads_as_such() {
    let (columns, _) = support::batches("SELECT 1 AS a");
    let mut store = ResultStore::new(columns);
    store.finish(StatementEnd::Stopped {
        rows: 2048,
        reason: StopReason::MemoryBudget,
    });
    let r = StatementResult {
        stmt: 0,
        sql: "SELECT 1".into(),
        engine: None,
        epoch: 0,
        store,
        elapsed: None,
        source: None,
        plan: None,
    };
    assert_eq!(summary(&r), "2,048 rows (memory budget)");
    let end = StatementEnd::Stopped {
        rows: 1,
        reason: StopReason::MemoryBudget,
    };
    assert_eq!(
        dbento_app::bench_driver::end_label(&end),
        "Stopped: memory budget"
    );
}
#[gpui_kit::test]
fn the_status_bar_names_the_budget(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    o.model(cx).update(cx, |m, _| m.set_max_result_bytes(1));
    o.run(cx, MANY);
    o.idle(cx);
    assert_eq!(
        label_of(cx, o.win, "status-truncated").as_deref(),
        Some("memory budget reached at 0 rows")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_held_batch_past_the_budget_parks_and_stops_on_the_pull() {
    let (_, core) = tokio::task::spawn_blocking(|| support::batches(MANY))
        .await
        .unwrap();
    let budget = (core[0].heap_bytes() + core[1].heap_bytes()) as u64; // exactly two batches
    let fx = fixture("", "");
    let parks = RunnerTuning {
        first_rows: 200,
        ..eager()
    };
    let r = spawn_tuned(&here(), spec(&fx, "lite"), parks);
    let ev = r.events();
    r.send(run(1, MANY.into(), budget));
    // Two batches go out, the third is read ahead and held: the run parks, not stops.
    let (_, rows) = until_event(&ev, |e| matches!(e, Event::Paused { run_id: 1, .. })).await;
    assert_eq!(rows, 2048);
    r.send(Command::Pull { run_id: 1 });
    match support::next_event(&ev).await {
        Event::StatementDone {
            end:
                StatementEnd::Stopped {
                    rows: 2048,
                    reason: StopReason::MemoryBudget,
                },
            ..
        } => {}
        other @ (Event::Connected { .. }
        | Event::ConnectFailed { .. }
        | Event::Executing { .. }
        | Event::StatementStarted { .. }
        | Event::NeedsConfirm { .. }
        | Event::Planned { .. }
        | Event::Batch { .. }
        | Event::Paused { .. }
        | Event::ParkedAtCap { .. }
        | Event::ParkExpired { .. }
        | Event::ParkReleased { .. }
        | Event::StatementDone { .. }
        | Event::RunDone { .. }
        | Event::Stopped { .. }
        | Event::SettingsChanged { .. }
        | Event::SettingsLost { .. }
        | Event::Disconnected { .. }
        | Event::Target { .. }
        | Event::Settling { .. }
        | Event::SettleFailed { .. }) => {
            panic!("the held batch was sent or not weighed: {other:?}")
        }
    }
}
