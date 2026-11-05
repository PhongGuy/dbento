//! The plan tree's keys, labels and highlight, and the view inside the results pane.
mod support;
use dbento_app::plan_view::{HOT_SHARE, PlanTree, PlanView, TreeKey, header_text};
use dbento_app::tab_model::PlanResult;
use dbento_core::{Engine, ExplainMode, ExplainOutput, Plan, explain_request};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext};
use std::sync::Arc;
use support::{init, label_of, opened, present};

const ANALYZE: &str =
    include_str!("../../dbento-core/tests/fixtures/explain/pg17_analyze_buffers.json");
fn tree() -> PlanTree {
    PlanTree::new(Arc::new(Plan::from_postgres_json(ANALYZE).unwrap()))
}

#[test]
fn everything_starts_expanded_and_keys_walk_the_tree() {
    let mut t = tree();
    assert_eq!((t.visible(), t.selected()), (vec![0, 1, 2, 3, 4], Some(0)));
    for _ in 0..3 {
        t.key(TreeKey::Down);
    }
    assert_eq!(t.selected(), Some(3));
    t.key(TreeKey::Left);
    assert_eq!((t.visible(), t.selected()), (vec![0, 1, 2, 3], Some(3)));
    t.key(TreeKey::Left);
    assert_eq!(t.selected(), Some(1));
    t.key(TreeKey::Down);
    t.key(TreeKey::Down);
    t.key(TreeKey::Right);
    assert_eq!(t.visible(), [0, 1, 2, 3, 4]);
    t.key(TreeKey::Right);
    assert_eq!(t.selected(), Some(4));
    t.key(TreeKey::Down);
    assert_eq!(t.selected(), Some(4));
    for _ in 0..6 {
        t.key(TreeKey::Up);
    }
    assert_eq!(t.selected(), Some(0));
    t.key(TreeKey::Toggle);
    assert_eq!(t.visible(), [0]);
    t.key(TreeKey::Toggle);
    assert_eq!(t.visible().len(), 5);
}

#[test]
fn only_nodes_over_a_quarter_of_execution_are_hot() {
    let t = tree();
    // Seq Scan on t owns 1.458 of 3.062 ms; the next largest, Hash Join, 0.523.
    assert_eq!((0..5).filter(|&i| t.hot(i)).collect::<Vec<_>>(), [4]);
    assert_eq!(HOT_SHARE, 0.25);
}

#[test]
fn labels_detail_and_header_read_from_the_node() {
    let t = tree();
    assert_eq!(t.row_label(4), "Seq Scan on t");
    assert_eq!(t.row_label(1), "Hash Join (Inner)");
    let d = t.detail_line(4);
    assert!(
        d.starts_with(
            "Filter: (a < 10) · Rows Removed by Filter: 18000 · Shared Hit: 187 · Shared Read: 0"
        ),
        "{d}"
    );
    assert_eq!(
        header_text(t.plan()).as_deref(),
        Some("Planning 1.228 ms · Execution 3.062 ms")
    );
    let lite = Plan::from_sqlite_rows(&[(2, 0, "SCAN t".to_owned())]);
    assert_eq!(header_text(&lite), None);
    assert_eq!(PlanTree::new(Arc::new(lite)).row_label(0), "SCAN t");
}

#[gpui_kit::test]
fn a_plan_result_shows_the_tree_then_raw_and_a_run_shows_the_grid(cx: &mut TestAppContext) {
    let o = opened(cx, "CREATE TABLE t(x); INSERT INTO t VALUES (1);", "", 0);
    let m = o.model(cx);
    let req = explain_request(
        Engine::Sqlite,
        "SELECT * FROM t WHERE x > 0",
        ExplainMode::Analyze,
    )
    .unwrap();
    m.update(cx, |m, cx| assert!(m.request_explain(req, 0, cx)));
    o.idle(cx);
    for id in [
        "plan-view",
        "plan-header",
        "plan-tree",
        "plan-note",
        "plan-detail",
    ] {
        assert!(present(cx, o.win, id), "{id}");
    }
    assert!(present(cx, o.win, ("plan-node", 0usize)));
    assert!(
        label_of(cx, o.win, ("plan-node", 0usize))
            .unwrap()
            .starts_with("SCAN t")
    );
    cx.update_window(o.win, |_, w, cx| w.click("plan-toggle-raw", cx))
        .unwrap();
    assert!(present(cx, o.win, "plan-raw") && !present(cx, o.win, "plan-tree"));
    assert!(present(cx, o.win, "plan-copy"));
    o.run(cx, "SELECT 1");
    o.idle(cx);
    assert!(present(cx, o.win, "result-grid") && !present(cx, o.win, "plan-view"));
}

#[gpui_kit::test]
fn output_that_does_not_parse_opens_raw_with_the_error(cx: &mut TestAppContext) {
    init(cx);
    let mut result = PlanResult::new(ExplainMode::Plan, false);
    result.output = Some(Arc::new(ExplainOutput::from_postgres("{".to_owned())));
    let win = cx.add_window(|window, cx| PlanView::new(&result, window, cx));
    assert!(present(cx, win.into(), "plan-parse-error"));
    assert!(present(cx, win.into(), "plan-raw"));
    assert!(!present(cx, win.into(), "plan-tree"));
    let pending = PlanResult::new(ExplainMode::Plan, false);
    let win = cx.add_window(|window, cx| PlanView::new(&pending, window, cx));
    assert!(present(cx, win.into(), "plan-pending"));
}

#[gpui_kit::test]
fn a_failed_explain_shows_the_error_not_the_plan_view(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let m = o.model(cx);
    let req = explain_request(Engine::Sqlite, "SELECT * FROM missing", ExplainMode::Plan).unwrap();
    m.update(cx, |m, cx| assert!(m.request_explain(req, 0, cx)));
    o.idle(cx);
    assert!(present(cx, o.win, "result-error"));
    assert!(!present(cx, o.win, "plan-view"));
}

#[gpui_kit::test]
fn the_explorer_tree_keys_walk_the_plan_tree_once_it_has_focus(cx: &mut TestAppContext) {
    init(cx);
    let mut result = PlanResult::new(ExplainMode::Analyze, false);
    result.output = Some(Arc::new(ExplainOutput::from_postgres(ANALYZE.to_owned())));
    let win = cx.add_window(|window, cx| PlanView::new(&result, window, cx));
    let selected = |cx: &mut TestAppContext| {
        win.read_with(cx, |v, _| v.tree().and_then(PlanTree::selected))
            .unwrap()
    };
    assert!(present(cx, win.into(), ("plan-node", 0usize)));
    cx.update_window(win.into(), |_, w, cx| w.click(("plan-node", 0usize), cx))
        .unwrap();
    cx.simulate_keystrokes(win.into(), "down");
    assert_eq!(selected(cx), Some(1));
    cx.simulate_keystrokes(win.into(), "end");
    assert_eq!(selected(cx), Some(4));
    cx.simulate_keystrokes(win.into(), "home");
    assert_eq!(selected(cx), Some(0));
    cx.simulate_keystrokes(win.into(), "left");
    assert_eq!(
        win.read_with(cx, |v, _| v.tree().unwrap().visible())
            .unwrap(),
        [0]
    );
    cx.simulate_keystrokes(win.into(), "right right");
    assert_eq!(selected(cx), Some(1));
}

#[test]
fn every_postgres_row_shows_the_estimate_and_analyze_adds_the_actuals() {
    let t = tree();
    assert_eq!(
        t.figures(4).as_deref(),
        Some("cost 0.00..437.00 · rows 2000 · width 37 · actual 1.458 ms · rows 2000 · loops 1")
    );
    const PLAIN: &str = include_str!("../../dbento-core/tests/fixtures/explain/pg17_plain.json");
    let plain = PlanTree::new(Arc::new(Plan::from_postgres_json(PLAIN).unwrap()));
    assert_eq!(
        plain.figures(0).as_deref(),
        Some("cost 0.29..8.30 · rows 1 · width 41")
    );
    let lite = PlanTree::new(Arc::new(Plan::from_sqlite_rows(&[(
        2,
        0,
        "SCAN t".to_owned(),
    )])));
    assert_eq!(lite.figures(0), None);
}

#[gpui_kit::test]
fn a_plan_result_hides_the_export_button(cx: &mut TestAppContext) {
    let o = opened(cx, "CREATE TABLE t(x); INSERT INTO t VALUES (1);", "", 0);
    let m = o.model(cx);
    let req = explain_request(Engine::Sqlite, "SELECT * FROM t", ExplainMode::Plan).unwrap();
    m.update(cx, |m, cx| assert!(m.request_explain(req, 0, cx)));
    o.idle(cx);
    assert!(present(cx, o.win, "plan-view") && !present(cx, o.win, "result-export"));
    o.run(cx, "SELECT 1");
    o.idle(cx);
    assert!(present(cx, o.win, "result-export"));
}

#[gpui_kit::test]
fn a_narrow_plan_header_keeps_the_toggle_whole(cx: &mut TestAppContext) {
    init(cx);
    let mut result = PlanResult::new(ExplainMode::Analyze, false);
    result.output = Some(Arc::new(ExplainOutput::from_postgres(ANALYZE.to_owned())));
    let win: gpui_kit::AnyWindowHandle = cx
        .add_window(|window, cx| PlanView::new(&result, window, cx))
        .into();
    let toggle_at = |cx: &mut TestAppContext, width: f32| {
        cx.simulate_window_resize(win, gpui_kit::size(gpui_kit::px(width), gpui_kit::px(400.)));
        cx.update_window(win, |_, w, cx| {
            w.render_frame(cx);
            w.find("plan-toggle").bounds().size
        })
        .unwrap()
    };
    let wide = toggle_at(cx, 1200.);
    let narrow = toggle_at(cx, 200.);
    assert_eq!(narrow, wide, "the toggle shrank in a narrow header");
}
