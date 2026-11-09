use crate::support;
use crate::support::{LONG, fixture, init_tokio, opened, spec, wait_real};
use dbento_app::app::AppPaths;
use dbento_app::history::{HistoryLog, history_outcome};
use dbento_app::runner::{Event, StatementEnd, StopReason};
use dbento_app::tab_model::TabModel;
use dbento_core::{ExecuteError, History, HistoryEntry, HistoryFilter, HistoryOutcome};
use gpui_kit::{AppContext as _, Entity, TestAppContext};
use std::path::{Path, PathBuf};
use std::time::Duration;

const T: &str = "CREATE TABLE t(x); INSERT INTO t VALUES (1),(2),(3);";

fn entries(path: &Path) -> Vec<HistoryEntry> {
    History::open(path)
        .map(|h| h.recent(&HistoryFilter::default(), 100))
        .unwrap_or_default()
}
fn logged(cx: &mut TestAppContext, fx: &support::Fx, dir: &Path) -> (Entity<TabModel>, PathBuf) {
    init_tokio(cx);
    let path = dir.join("history.jsonl");
    let log = cx.new(|_| HistoryLog::new(path.clone()));
    let m = cx.new(|_| TabModel::new(spec(fx, "lite")));
    m.update(cx, |m, _| m.set_history(log));
    (m, path)
}

#[gpui_kit::test]
fn each_statement_of_a_run_is_recorded_once_with_its_outcome(cx: &mut TestAppContext) {
    let (fx, dir) = (fixture(T, ""), tempfile::tempdir().unwrap());
    let (m, path) = logged(cx, &fx, dir.path());
    m.update(cx, |m, cx| m.run("SELECT x FROM t; SELEC 1".into(), cx));
    wait_real(cx, "the run", |cx| m.read_with(cx, |m, _| !m.running()));
    wait_real(cx, "two entries", |_| entries(&path).len() == 2);
    let e = entries(&path);
    assert_eq!(
        (e[1].sql(), e[1].connection(), e[1].workspace()),
        ("SELECT x FROM t", "lite", fx.ws.dir())
    );
    assert_eq!(
        e[1].outcome(),
        &HistoryOutcome::Ok {
            rows: 3,
            truncated: false,
            rows_affected: None
        }
    );
    assert_eq!(e[0].sql(), "SELEC 1");
    assert!(matches!(e[0].outcome(), HistoryOutcome::Failed { message } if !message.is_empty()));
    cx.run_until_parked();
    assert_eq!(entries(&path).len(), 2, "nothing is recorded twice");
}

#[gpui_kit::test]
fn a_skip_and_a_settle_failure_are_recorded(cx: &mut TestAppContext) {
    let (fx, dir) = (fixture(T, ""), tempfile::tempdir().unwrap());
    let (m, path) = logged(cx, &fx, dir.path());
    m.update(cx, |m, cx| {
        m.apply(Event::Executing {
            run_id: 0,
            stmt: 0,
            sql: "UPDATE t SET x = 0".into(),
        });
        m.apply(Event::StatementDone {
            run_id: 0,
            stmt: 0,
            end: StatementEnd::Skipped,
            elapsed: Duration::ZERO,
        });
        m.apply(Event::SettleFailed {
            message: "deferred constraint failed".into(),
        });
        m.flush_history(cx);
    });
    wait_real(cx, "two entries", |_| entries(&path).len() == 2);
    let e = entries(&path);
    assert_eq!(
        (e[1].sql(), e[1].outcome()),
        ("UPDATE t SET x = 0", &HistoryOutcome::Skipped)
    );
    assert_eq!(e[0].sql(), "UPDATE t SET x = 0");
    assert!(
        matches!(e[0].outcome(), HistoryOutcome::Failed { message } if message.contains("deferred constraint failed"))
    );
}

#[gpui_kit::test]
fn a_statement_cut_by_closing_the_tab_is_recorded_as_cancelled(cx: &mut TestAppContext) {
    let (fx, dir) = (fixture("", ""), tempfile::tempdir().unwrap());
    let (m, path) = logged(cx, &fx, dir.path());
    m.update(cx, |m, cx| m.run(LONG.into(), cx));
    wait_real(cx, "the running row", |cx| {
        m.read_with(cx, |m, _| !m.results().is_empty())
    });
    let _closing = m.update(cx, |m, cx| m.close(cx));
    wait_real(cx, "the entry", |_| entries(&path).len() == 1);
    assert_eq!(entries(&path)[0].outcome(), &HistoryOutcome::Cancelled);
}

#[gpui_kit::test]
fn a_statement_replaced_by_a_new_run_is_recorded_as_cancelled(cx: &mut TestAppContext) {
    let (fx, dir) = (fixture("", ""), tempfile::tempdir().unwrap());
    let (m, path) = logged(cx, &fx, dir.path());
    // A cap above MANY's rows keeps the stream open, so it parks instead of truncating.
    m.update(cx, |m, _| m.set_max_rows(Some(10_000)));
    m.update(cx, |m, cx| m.run(support::MANY.into(), cx));
    wait_real(cx, "the park", |cx| {
        m.read_with(cx, |m, _| m.paused().is_some())
    });
    m.update(cx, |m, cx| m.run("SELECT 1".into(), cx));
    wait_real(cx, "the run", |cx| m.read_with(cx, |m, _| !m.running()));
    wait_real(cx, "two entries", |_| entries(&path).len() == 2);
    let e = entries(&path);
    assert!(e[1].sql().starts_with("WITH RECURSIVE"), "{}", e[1].sql());
    assert_eq!(e[1].outcome(), &HistoryOutcome::Cancelled);
    assert_eq!(e[0].sql(), "SELECT 1");
    cx.run_until_parked();
    assert_eq!(
        entries(&path).len(),
        2,
        "the replaced run's late end adds nothing"
    );
}

#[gpui_kit::test]
fn a_statement_lost_with_its_runner_is_recorded_as_failed(cx: &mut TestAppContext) {
    let (fx, dir) = (fixture("", ""), tempfile::tempdir().unwrap());
    let (m, path) = logged(cx, &fx, dir.path());
    m.update(cx, |m, cx| {
        m.apply(Event::Executing {
            run_id: 0,
            stmt: 0,
            sql: "SELECT 1".into(),
        });
        m.apply(Event::Stopped {
            message: "session stopped".into(),
        });
        m.flush_history(cx);
    });
    wait_real(cx, "the entry", |_| entries(&path).len() == 1);
    assert_eq!(
        entries(&path)[0].outcome(),
        &HistoryOutcome::Failed {
            message: "session stopped".into()
        }
    );
}

#[test]
fn stopped_and_failed_ends_map_to_their_outcomes() {
    let stopped = |reason| StatementEnd::Stopped { rows: 5, reason };
    assert_eq!(
        history_outcome(&StatementEnd::Skipped),
        HistoryOutcome::Skipped
    );
    for r in [StopReason::User, StopReason::NewRun, StopReason::Closed] {
        assert_eq!(history_outcome(&stopped(r)), HistoryOutcome::Cancelled);
    }
    assert_eq!(
        history_outcome(&stopped(StopReason::MemoryBudget)),
        HistoryOutcome::Ok {
            rows: 5,
            truncated: true,
            rows_affected: None
        }
    );
    assert_eq!(
        history_outcome(&stopped(StopReason::Lost)),
        HistoryOutcome::Failed {
            message: "session stopped".into()
        }
    );
    assert_eq!(
        history_outcome(&StatementEnd::Failed(ExecuteError::Cancelled)),
        HistoryOutcome::Cancelled
    );
}

#[gpui_kit::test]
fn the_app_records_into_its_history_file(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    o.run(cx, "SELECT 1");
    o.idle(cx);
    let path = AppPaths::in_dir(o.files.path()).history;
    wait_real(cx, "the entry", |_| entries(&path).len() == 1);
    assert_eq!(entries(&path)[0].sql(), "SELECT 1");
}
