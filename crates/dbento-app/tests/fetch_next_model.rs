//! The tab model's state at the row cap, and the history entry a capped result leaves.
mod support;
use dbento_app::history::HistoryLog;
use dbento_app::runner::{Event, RunnerTuning, StatementEnd, StopReason};
use dbento_app::tab_model::{CapView, Expiry, TabModel};
use dbento_core::{History, HistoryEntry, HistoryFilter, HistoryOutcome};
use gpui_kit::{AppContext as _, Entity, TestAppContext};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};
use support::{MANY, fixture, init_tokio, spec, wait_real};

const LONG_PARK: Duration = Duration::from_secs(30);

fn entries(path: &Path) -> Vec<HistoryEntry> {
    History::open(path)
        .map(|h| h.recent(&HistoryFilter::default(), 100))
        .unwrap_or_default()
}
fn ok(rows: u64, truncated: bool) -> HistoryOutcome {
    HistoryOutcome::Ok {
        rows,
        truncated,
        rows_affected: None,
    }
}
/// A model over `lite` with a 1,000-row cap, a history log and a park of `park`.
fn capped(
    cx: &mut TestAppContext,
    fx: &support::Fx,
    dir: &Path,
    park: Duration,
) -> (Entity<TabModel>, PathBuf) {
    init_tokio(cx);
    let path = dir.join("history.jsonl");
    let log = cx.new(|_| HistoryLog::new(path.clone()));
    let tuning = RunnerTuning {
        park_at_cap: park,
        ..RunnerTuning::default()
    };
    let m = cx.new(|_| TabModel::with_tuning(spec(fx, "lite"), tuning));
    m.update(cx, |m, _| {
        m.set_history(log);
        m.set_max_rows(Some(1_000));
    });
    (m, path)
}
fn park(cx: &mut TestAppContext, m: &Entity<TabModel>) {
    m.update(cx, |m, cx| m.run(MANY.into(), cx));
    wait_real(cx, "the park at the cap", |cx| {
        m.read_with(cx, |m, _| m.parked_at_cap().is_some())
    });
}
fn ended(cx: &mut TestAppContext, m: &Entity<TabModel>) {
    wait_real(cx, "the end", |cx| {
        m.read_with(cx, |m, _| {
            m.results().first().is_some_and(|r| r.store.end().is_some())
        })
    });
}

#[gpui_kit::test]
fn at_the_cap_the_model_is_parked_not_running_and_fetching_is_running(cx: &mut TestAppContext) {
    let (fx, dir) = (fixture("", ""), tempfile::tempdir().unwrap());
    let (m, _) = capped(cx, &fx, dir.path(), LONG_PARK);
    park(cx, &m);
    m.read_with(cx, |m, _| {
        assert_eq!(m.parked_at_cap(), Some(0));
        assert_eq!(m.cap_view(0), Some(CapView::Parked { rows: 1000 }));
        assert!(!m.running());
        assert!(!m.has_more(), "scrolling sends no Pull at the cap");
        assert!(m.results()[0].store.end().is_none());
        assert!(
            m.results()[0].elapsed.is_some(),
            "the park reports its time so far"
        );
    });
    m.update(cx, |m, _| {
        m.set_max_rows(Some(100_000)); // the selector moves; Fetch next keeps the run's 1,000
        m.fetch_next();
        assert_eq!(m.cap_view(0), Some(CapView::Fetching));
        assert_eq!(m.parked_at_cap(), None);
        assert!(m.running(), "Stop stays live while fetching");
    });
    wait_real(cx, "the second park", |cx| {
        m.read_with(cx, |m, _| m.parked_at_cap().is_some())
    });
    m.read_with(cx, |m, _| {
        assert_eq!(m.results()[0].store.rows(), 2000);
        assert_eq!(m.cap_view(0), Some(CapView::Parked { rows: 2000 }));
    });
}

#[gpui_kit::test]
fn fetch_all_records_one_untruncated_entry_when_the_stream_ends(cx: &mut TestAppContext) {
    let (fx, dir) = (fixture("", ""), tempfile::tempdir().unwrap());
    let (m, path) = capped(cx, &fx, dir.path(), LONG_PARK);
    park(cx, &m);
    std::thread::sleep(Duration::from_millis(300));
    cx.run_until_parked();
    assert!(entries(&path).is_empty(), "nothing is written at the park");
    m.update(cx, |m, _| m.fetch_all());
    ended(cx, &m);
    wait_real(cx, "one entry", |_| entries(&path).len() == 1);
    assert_eq!(entries(&path)[0].outcome(), &ok(5000, false));
    m.read_with(cx, |m, _| {
        assert_eq!(m.cap_view(0), Some(CapView::Ended { expired: None }))
    });
}

#[gpui_kit::test]
fn a_new_run_over_a_capped_result_records_it_truncated(cx: &mut TestAppContext) {
    let (fx, dir) = (fixture("", ""), tempfile::tempdir().unwrap());
    let (m, path) = capped(cx, &fx, dir.path(), LONG_PARK);
    park(cx, &m);
    m.update(cx, |m, cx| m.run("SELECT 1".into(), cx));
    wait_real(cx, "the run", |cx| m.read_with(cx, |m, _| !m.running()));
    wait_real(cx, "two entries", |_| entries(&path).len() == 2);
    let e = entries(&path);
    assert!(e[1].sql().starts_with("WITH RECURSIVE"), "{}", e[1].sql());
    assert_eq!(e[1].outcome(), &ok(1000, true));
    assert_eq!(e[0].sql(), "SELECT 1");
    cx.run_until_parked();
    assert_eq!(
        entries(&path).len(),
        2,
        "the replaced run's late end adds nothing"
    );
}

#[gpui_kit::test]
fn closing_a_capped_result_ends_it_truncated(cx: &mut TestAppContext) {
    let (fx, dir) = (fixture("", ""), tempfile::tempdir().unwrap());
    let (m, path) = capped(cx, &fx, dir.path(), LONG_PARK);
    park(cx, &m);
    let _closing = m.update(cx, |m, cx| m.close(cx));
    m.read_with(cx, |m, _| {
        let end = m.results()[0].store.end();
        assert!(
            matches!(end, Some(StatementEnd::Truncated(o)) if o.rows == 1000),
            "{end:?}"
        );
        assert_eq!(m.cap_view(0), Some(CapView::Ended { expired: None }));
    });
    wait_real(cx, "the entry", |_| entries(&path).len() == 1);
    assert_eq!(entries(&path)[0].outcome(), &ok(1000, true));
}

#[gpui_kit::test]
fn reconnecting_over_a_capped_result_ends_it_truncated(cx: &mut TestAppContext) {
    let (fx, dir) = (fixture("", ""), tempfile::tempdir().unwrap());
    let (m, path) = capped(cx, &fx, dir.path(), LONG_PARK);
    park(cx, &m);
    let ws = fx.ws.clone();
    let _closing = m.update(cx, |m, cx| m.reconnect(ws, cx));
    m.read_with(cx, |m, _| {
        let end = m.results()[0].store.end();
        assert!(
            matches!(end, Some(StatementEnd::Truncated(o)) if o.rows == 1000),
            "{end:?}"
        );
        assert_eq!(m.cap_view(0), Some(CapView::Ended { expired: None }));
        assert!(!m.closed(), "a reconnect keeps the model open");
    });
    wait_real(cx, "the entry", |_| entries(&path).len() == 1);
    assert_eq!(entries(&path)[0].outcome(), &ok(1000, true));
    m.update(cx, |m, cx| m.run("SELECT 1".into(), cx));
    wait_real(cx, "the next run", |cx| {
        m.read_with(cx, |m, _| {
            m.results().first().is_some_and(|r| r.store.end().is_some())
        })
    });
}

#[gpui_kit::test]
fn stop_at_the_cap_ends_truncated(cx: &mut TestAppContext) {
    let (fx, dir) = (fixture("", ""), tempfile::tempdir().unwrap());
    let (m, path) = capped(cx, &fx, dir.path(), LONG_PARK);
    park(cx, &m);
    m.update(cx, |m, _| m.cancel());
    ended(cx, &m);
    m.read_with(cx, |m, _| {
        let end = m.results()[0].store.end();
        assert!(
            matches!(end, Some(StatementEnd::Truncated(o)) if o.rows == 1000),
            "{end:?}"
        );
    });
    wait_real(cx, "the entry", |_| entries(&path).len() == 1);
    assert_eq!(entries(&path)[0].outcome(), &ok(1000, true));
}

#[gpui_kit::test]
fn a_fetch_after_stop_at_the_cap_sends_nothing(cx: &mut TestAppContext) {
    let (fx, dir) = (fixture("", ""), tempfile::tempdir().unwrap());
    let (m, _path) = capped(cx, &fx, dir.path(), LONG_PARK);
    park(cx, &m);
    m.update(cx, |m, _| {
        assert_eq!(
            m.read_only(),
            Some(false),
            "a read-write Stop waits for the end"
        );
        m.cancel();
        m.fetch_next();
        m.fetch_all();
        assert_eq!(m.cap_view(0), Some(CapView::Parked { rows: 1000 }));
    });
    ended(cx, &m);
    m.read_with(cx, |m, _| {
        let end = m.results()[0].store.end();
        assert!(
            matches!(end, Some(StatementEnd::Truncated(o)) if o.rows == 1000),
            "{end:?}"
        );
    });
}

#[gpui_kit::test]
fn an_expired_park_is_marked_and_recorded_truncated(cx: &mut TestAppContext) {
    let (fx, dir) = (fixture("", ""), tempfile::tempdir().unwrap());
    let (m, path) = capped(cx, &fx, dir.path(), Duration::from_millis(300));
    m.update(cx, |m, cx| m.run(MANY.into(), cx));
    ended(cx, &m);
    m.read_with(cx, |m, _| {
        assert_eq!(
            m.cap_view(0),
            Some(CapView::Ended {
                expired: Some(Expiry::Idle)
            })
        );
        assert!(
            matches!(m.results()[0].store.end(), Some(StatementEnd::Truncated(o)) if o.rows == 1000)
        );
    });
    wait_real(cx, "the entry", |_| entries(&path).len() == 1);
    assert_eq!(entries(&path)[0].outcome(), &ok(1000, true));
}

#[gpui_kit::test]
fn a_runner_lost_after_the_park_ends_lost(cx: &mut TestAppContext) {
    let (fx, dir) = (fixture("", ""), tempfile::tempdir().unwrap());
    let (m, _) = capped(cx, &fx, dir.path(), LONG_PARK);
    park(cx, &m);
    m.update(cx, |m, cx| {
        m.apply(Event::Stopped {
            message: "session stopped".into(),
        });
        cx.notify();
    });
    m.read_with(cx, |m, _| {
        assert!(matches!(
            m.results()[0].store.end(),
            Some(StatementEnd::Stopped {
                rows: 1000,
                reason: StopReason::Lost
            })
        ));
        assert_eq!(m.cap_view(0), Some(CapView::Ended { expired: None }));
    });
}

#[gpui_kit::test]
fn a_fetch_in_flight_keeps_its_time_ticking(cx: &mut TestAppContext) {
    let (fx, dir) = (fixture("", ""), tempfile::tempdir().unwrap());
    let (m, _) = capped(cx, &fx, dir.path(), LONG_PARK);
    park(cx, &m);
    m.update(cx, |m, _| {
        let at_park = m.results()[0].elapsed.expect("the park's time");
        let later = Instant::now() + Duration::from_secs(5);
        assert_eq!(m.elapsed_at(0, later), Some(at_park), "a park stands still");
        m.fetch_next();
        let ticking = m.elapsed_at(0, later).unwrap();
        assert!(ticking > at_park + Duration::from_secs(4), "{ticking:?}");
    });
}
