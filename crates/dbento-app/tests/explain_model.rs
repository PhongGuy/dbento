//! The tab model's plan results: one per explain, finished, kept out of history, at the user's text.
use crate::support;
use crate::support::{fixture, init_tokio, spec, wait_real};
use dbento_app::history::HistoryLog;
use dbento_app::runner::StatementEnd;
use dbento_app::tab_model::TabModel;
use dbento_core::{Engine, ExplainMode, History, HistoryEntry, HistoryFilter, explain_request};
use gpui_kit::{AppContext as _, Entity, TestAppContext};
use std::path::Path;

fn entries(path: &Path) -> Vec<HistoryEntry> {
    History::open(path)
        .map(|h| h.recent(&HistoryFilter::default(), 100))
        .unwrap_or_default()
}

fn model(cx: &mut TestAppContext, fx: &support::Fx) -> Entity<TabModel> {
    init_tokio(cx);
    cx.new(|_| TabModel::new(spec(fx, "lite")))
}
fn explain(
    cx: &mut TestAppContext,
    m: &Entity<TabModel>,
    sql: &str,
    start: usize,
    mode: ExplainMode,
) {
    let req = explain_request(Engine::Sqlite, sql, mode).unwrap();
    m.update(cx, |m, cx| {
        assert!(m.request_explain(req, start, cx), "the explain starts")
    });
    wait_real(cx, "the plan", |cx| m.read_with(cx, |m, _| !m.running()));
}

#[gpui_kit::test]
fn an_explain_keeps_one_finished_plan_result_at_the_users_text(cx: &mut TestAppContext) {
    let fx = fixture("CREATE TABLE t(x); INSERT INTO t VALUES (1),(2);", "");
    let m = model(cx, &fx);
    // The statement sits at offset 10 of the editor, after "SELECT 1;\n".
    explain(
        cx,
        &m,
        "SELECT x FROM t WHERE x > 1",
        10,
        ExplainMode::Analyze,
    );
    m.read_with(cx, |m, _| {
        assert_eq!(m.results().len(), 1);
        let r = &m.results()[0];
        assert_eq!(r.sql, "SELECT x FROM t WHERE x > 1");
        assert_eq!(r.source, Some(10..37));
        assert!(matches!(r.store.end(), Some(StatementEnd::Planned)));
        assert!(r.elapsed.is_some());
        let p = r.plan.as_ref().expect("a plan result");
        assert!(p.analyze_unavailable);
        assert_eq!(p.mode, ExplainMode::Plan);
        assert!(p.output.as_ref().unwrap().raw.contains("SCAN t"));
    });
}

#[gpui_kit::test]
fn a_failed_explain_ends_failed(cx: &mut TestAppContext) {
    let fx = fixture("", "");
    let m = model(cx, &fx);
    explain(cx, &m, "SELECT * FROM missing", 0, ExplainMode::Plan);
    m.read_with(cx, |m, _| {
        let r = &m.results()[0];
        assert!(
            matches!(r.store.end(), Some(StatementEnd::Failed(_))),
            "{:?}",
            r.store.end()
        );
        assert!(r.plan.as_ref().unwrap().output.is_none());
    });
}

#[gpui_kit::test]
fn only_the_normal_run_after_an_explain_reaches_history(cx: &mut TestAppContext) {
    let (fx, dir) = (
        fixture("CREATE TABLE t(x);", ""),
        tempfile::tempdir().unwrap(),
    );
    init_tokio(cx);
    let path = dir.path().join("history.jsonl");
    let log = cx.new(|_| HistoryLog::new(path.clone()));
    let m = cx.new(|_| TabModel::new(spec(&fx, "lite")));
    m.update(cx, |m, _| m.set_history(log));
    explain(cx, &m, "SELECT * FROM t", 0, ExplainMode::Plan);
    m.update(cx, |m, cx| m.run("SELECT 42".into(), cx));
    wait_real(cx, "the run", |cx| m.read_with(cx, |m, _| !m.running()));
    wait_real(cx, "an entry", |_| !entries(&path).is_empty());
    cx.run_until_parked();
    let e = entries(&path);
    assert_eq!(e.iter().map(|e| e.sql()).collect::<Vec<_>>(), ["SELECT 42"]);
    m.read_with(cx, |m, _| assert!(m.results()[0].plan.is_none()));
}
