//! A fenced model sends nothing new to its old session; a running statement and Stop go on,
//! and Reconnect or a switch lift the fence before connecting.
use crate::support::{
    LONG, MANY, browsing, fixture, init_tokio, ready, spec, wait_prompt, wait_real,
};
use dbento_app::runner::RunnerTuning;
use dbento_app::tab_model::{Fence, SaveState, TabModel};
use dbento_core::{ColumnInfo, Engine, ExplainMode, RelationRef, explain_request};
use gpui_kit::{AppContext as _, Entity, TestAppContext};
use std::sync::Arc;
use std::time::Duration;

const T: &str = "CREATE TABLE t(id INTEGER PRIMARY KEY, x); INSERT INTO t VALUES (1, 1), (2, 2);";
const BIG: &str = "CREATE TABLE t(id INTEGER PRIMARY KEY, g INTEGER, v TEXT); \
    WITH RECURSIVE r(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM r WHERE x < 2500) \
    INSERT INTO t SELECT x, x % 7, 'v' || x FROM r;";

// No first-rows pause below the cap, and parks that expire after 300 ms.
fn expiring() -> RunnerTuning {
    RunnerTuning {
        first_rows: 100_000,
        park_at_cap: Duration::from_millis(300),
        ..RunnerTuning::default()
    }
}
fn model(cx: &mut TestAppContext, fx: &crate::support::Fx) -> Entity<TabModel> {
    init_tokio(cx);
    cx.new(|_| TabModel::new(spec(fx, "lite")))
}
fn idle(cx: &mut TestAppContext, m: &Entity<TabModel>) {
    wait_real(cx, "the run", |cx| m.read_with(cx, |m, _| !m.running()));
}
fn fence(cx: &mut TestAppContext, m: &Entity<TabModel>, f: Option<Fence>) {
    m.update(cx, |m, cx| m.set_fence(f, cx));
}
fn pause(cx: &mut TestAppContext) {
    std::thread::sleep(Duration::from_millis(300));
    cx.run_until_parked();
}

#[gpui_kit::test]
fn a_fenced_model_refuses_every_session_entry(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = model(cx, &fx);
    m.update(cx, |m, cx| m.run("SELECT x FROM t".into(), cx));
    idle(cx, &m);
    fence(cx, &m, Some(Fence::Gone));
    let run_id = m.read_with(cx, |m, _| m.run_id());
    assert!(
        !m.update(cx, |m, cx| m.request_run("SELECT 2".into(), cx)),
        "run"
    );
    assert!(
        !m.update(cx, |m, cx| m.request_run_at("SELECT 2".into(), 0, cx)),
        "run at cursor"
    );
    let req = explain_request(Engine::Sqlite, "SELECT 1", ExplainMode::Plan).unwrap();
    assert!(
        !m.update(cx, |m, cx| m.request_explain(req, 0, cx)),
        "explain"
    );
    let cols: Arc<[ColumnInfo]> = m.read_with(cx, |m, _| m.results()[0].store.columns().into());
    let asked = m.update(cx, |m, cx| {
        m.source_columns(RelationRef::new("main", "t"), cols, cx)
    });
    assert!(asked.is_err(), "source columns");
    m.update(cx, |m, cx| m.load_target(RelationRef::new("main", "t"), cx));
    assert!(
        m.read_with(cx, |m, _| m.target_pending().is_none()),
        "browse target"
    );
    assert_eq!(
        m.read_with(cx, |m, _| (m.run_id(), m.running())),
        (run_id, false),
        "nothing started"
    );
    assert_eq!(m.read_with(cx, |m, _| m.fence()), Some(Fence::Gone));
    // Each refusal is the fence's: once lifted, the same calls go through.
    fence(cx, &m, None);
    let cols: Arc<[ColumnInfo]> = m.read_with(cx, |m, _| m.results()[0].store.columns().into());
    let asked = m.update(cx, |m, cx| {
        m.source_columns(RelationRef::new("main", "t"), cols, cx)
    });
    assert!(asked.is_ok(), "source columns once lifted");
    let req = explain_request(Engine::Sqlite, "SELECT 1", ExplainMode::Plan).unwrap();
    assert!(
        m.update(cx, |m, cx| m.request_explain(req, 0, cx)),
        "explain once lifted"
    );
    idle(cx, &m);
    assert!(
        m.update(cx, |m, cx| m.request_run_at("SELECT 2".into(), 0, cx)),
        "run at once lifted"
    );
    idle(cx, &m);
    assert!(
        m.update(cx, |m, cx| m.request_run("SELECT 2".into(), cx)),
        "run once lifted"
    );
    idle(cx, &m);
    m.update(cx, |m, cx| m.load_target(RelationRef::new("main", "t"), cx));
    assert!(
        m.read_with(cx, |m, _| m.target_pending().is_some()),
        "browse target once lifted"
    );
}

#[gpui_kit::test]
fn connect_is_refused_under_the_fence(cx: &mut TestAppContext) {
    let fx = fixture("", "");
    let m = model(cx, &fx);
    fence(cx, &m, Some(Fence::Gone));
    m.update(cx, |m, cx| m.connect(cx));
    pause(cx);
    assert_eq!(
        m.read_with(cx, |m, _| m.read_only()),
        None,
        "no session opened"
    );
    fence(cx, &m, None);
    m.update(cx, |m, cx| m.connect(cx));
    wait_real(cx, "the session once lifted", |cx| {
        m.read_with(cx, |m, _| m.read_only().is_some())
    });
}

#[gpui_kit::test]
fn run_again_is_refused_under_the_fence(cx: &mut TestAppContext) {
    let fx = fixture("", "");
    init_tokio(cx);
    let m = cx.new(|_| TabModel::with_tuning(spec(&fx, "lite"), expiring()));
    m.update(cx, |m, cx| m.run(MANY.into(), cx));
    wait_real(cx, "an expired, truncated result", |cx| {
        m.read_with(cx, |m, _| m.can_run_again())
    });
    fence(
        cx,
        &m,
        Some(Fence::Changed {
            now_read_only: false,
        }),
    );
    let run_id = m.read_with(cx, |m, _| m.run_id());
    assert!(!m.update(cx, |m, cx| m.run_again(cx)), "refused");
    assert_eq!(m.read_with(cx, |m, _| m.run_id()), run_id, "nothing sent");
    fence(cx, &m, None);
    assert!(m.update(cx, |m, cx| m.run_again(cx)), "allowed once lifted");
}

#[gpui_kit::test]
fn browse_sort_filter_and_continue_are_refused(cx: &mut TestAppContext) {
    let fx = fixture(BIG, "");
    let m = browsing(cx, &fx, "t", expiring(), "");
    wait_real(cx, "a continuable browse", |cx| {
        m.read_with(cx, |m, _| !m.running() && m.continuation().is_some())
    });
    let before = m.read_with(cx, |m, _| {
        (m.run_id(), m.browse().map(|b| b.sql().to_owned()))
    });
    fence(cx, &m, Some(Fence::Gone));
    assert!(!m.update(cx, |m, cx| m.continue_browse(cx)), "continue");
    assert!(!m.update(cx, |m, cx| m.cycle_sort("v", cx)), "sort");
    assert!(!m.update(cx, |m, cx| m.apply_filter("g = 1", cx)), "filter");
    assert!(
        !m.update(cx, |m, cx| m.request_clear_browse(cx)),
        "leave the browse"
    );
    pause(cx);
    let after = m.read_with(cx, |m, _| {
        (m.run_id(), m.browse().map(|b| b.sql().to_owned()))
    });
    assert_eq!(after, before, "the browse and its run are untouched");
    fence(cx, &m, None);
    assert!(
        m.update(cx, |m, cx| m.cycle_sort("v", cx)),
        "sort once lifted"
    );
}

#[gpui_kit::test]
fn a_queued_target_is_dropped_when_the_fence_lands(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = model(cx, &fx);
    m.update(cx, |m, cx| m.run(LONG.into(), cx));
    wait_real(cx, "the start", |cx| {
        m.read_with(cx, |m, _| m.running() && !m.results().is_empty())
    });
    m.update(cx, |m, cx| {
        m.load_target(RelationRef::new("main", "t"), cx);
        m.set_fence(Some(Fence::Gone), cx);
    });
    idle(cx, &m);
    pause(cx);
    let (pending, browse) = m.read_with(cx, |m, _| {
        (m.target_pending().is_some(), m.browse().is_some())
    });
    assert_eq!(
        (pending, browse),
        (false, false),
        "the queued target never went out"
    );
}

#[gpui_kit::test]
fn a_waiting_confirm_is_refused_but_cancel_still_answers(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = model(cx, &fx);
    m.update(cx, |m, cx| m.run("UPDATE t SET x = 0".into(), cx));
    wait_real(cx, "the question", |cx| {
        m.read_with(cx, |m, _| m.confirm().is_some())
    });
    fence(
        cx,
        &m,
        Some(Fence::Changed {
            now_read_only: true,
        }),
    );
    m.update(cx, |m, cx| m.answer(true, cx));
    assert!(
        m.read_with(cx, |m, _| m.confirm().is_some()),
        "confirm refused, still pending"
    );
    m.update(cx, |m, cx| m.answer(false, cx));
    idle(cx, &m);
    assert_eq!(
        fx.count("SELECT count(*) FROM t WHERE x = 0"),
        0,
        "nothing written"
    );
}

#[gpui_kit::test]
fn a_running_statement_goes_on_and_stop_still_works(cx: &mut TestAppContext) {
    let fx = fixture("", "");
    let m = model(cx, &fx);
    m.update(cx, |m, cx| m.run(LONG.into(), cx));
    wait_real(cx, "the start", |cx| {
        m.read_with(cx, |m, _| m.running() && !m.results().is_empty())
    });
    fence(
        cx,
        &m,
        Some(Fence::Changed {
            now_read_only: false,
        }),
    );
    pause(cx);
    assert!(
        m.read_with(cx, |m, _| m.running()),
        "the fence does not cut a run"
    );
    m.update(cx, |m, _| m.cancel());
    idle(cx, &m);
}

#[gpui_kit::test]
fn staged_edits_cannot_apply_or_commit_under_the_fence(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = ready(cx, &fx, "t");
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 1, "9", cx)), "staged");
    fence(cx, &m, Some(Fence::Gone));
    assert!(!m.update(cx, |m, cx| m.save(cx)), "apply refused");
    assert_eq!(m.read_with(cx, |m, _| m.save_state()), SaveState::Idle);
    fence(cx, &m, None);
    assert!(m.update(cx, |m, cx| m.save(cx)), "apply once lifted");
    wait_prompt(cx, &m);
    fence(cx, &m, Some(Fence::Gone));
    m.update(cx, |m, cx| m.commit(cx));
    assert!(
        m.read_with(cx, |m, _| m.apply_prompt().is_some()),
        "commit refused"
    );
    m.update(cx, |m, cx| m.rollback(cx));
    wait_real(cx, "the rollback", |cx| {
        m.read_with(cx, |m, _| m.save_state() == SaveState::Idle)
    });
    assert_eq!(
        fx.count("SELECT count(*) FROM t WHERE x = 9 OR x = '9'"),
        0,
        "nothing committed"
    );
}

#[gpui_kit::test]
fn reconnect_lifts_the_fence_before_it_connects(cx: &mut TestAppContext) {
    let fx = fixture("", "");
    let m = model(cx, &fx);
    fence(cx, &m, Some(Fence::Gone));
    let ws = Arc::clone(&fx.ws);
    drop(m.update(cx, |m, cx| m.reconnect(ws, cx)));
    assert_eq!(m.read_with(cx, |m, _| m.fence()), None);
    wait_real(cx, "the new session", |cx| {
        m.read_with(cx, |m, _| m.read_only().is_some())
    });
}

#[gpui_kit::test]
fn a_switch_lifts_the_fence_before_it_connects(cx: &mut TestAppContext) {
    let fx = fixture("", "");
    let m = model(cx, &fx);
    fence(cx, &m, Some(Fence::Gone));
    let ws = Arc::clone(&fx.ws);
    assert!(
        m.update(cx, |m, cx| m.switch_connection("nopg", ws, cx))
            .is_ok(),
        "switched"
    );
    assert_eq!(m.read_with(cx, |m, _| m.fence()), None);
    wait_real(cx, "the connect attempt", |cx| {
        m.read_with(cx, |m, _| {
            m.error().is_some() || m.connect_failure().is_some()
        })
    });
}

#[gpui_kit::test]
fn a_discard_question_waits_under_the_fence(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = ready(cx, &fx, "t");
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 1, "9", cx)), "staged");
    assert!(
        !m.update(cx, |m, cx| m.request_run("SELECT 2".into(), cx)),
        "asked"
    );
    let id = m.read_with(cx, |m, _| m.discard_question().map(|q| q.id));
    let id = id.expect("a discard question");
    fence(cx, &m, Some(Fence::Gone));
    let run_id = m.read_with(cx, |m, _| m.run_id());
    assert!(
        m.update(cx, |m, cx| m.answer_discard(id, true, cx))
            .is_none(),
        "discard refused"
    );
    let kept = m.read_with(cx, |m, _| {
        (m.discard_question().is_some(), m.edits().len(), m.run_id())
    });
    assert_eq!(
        kept,
        (true, 1, run_id),
        "the question, the edit and the run stay"
    );
    assert!(
        m.update(cx, |m, cx| m.answer_discard(id, false, cx))
            .is_some(),
        "keep still answers"
    );
}
