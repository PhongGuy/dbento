mod support;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext};
use support::{Opened, opened};

const TWO: &str = "SELECT 1 AS a;\nSELECT 2 AS b;";

/// `sql` in the focused editor with `from..to` selected.
fn place(cx: &mut TestAppContext, o: &Opened, sql: &str, from: usize, to: usize) {
    let tab = o.shell.read_with(cx, |s, _| s.tab().unwrap().clone());
    let e = o
        .shell
        .read_with(cx, |s, cx| s.tab().unwrap().read(cx).editor().clone());
    cx.update_window(o.win, |_, w, cx| {
        tab.update(cx, |t, cx| t.set_sql(sql, w, cx));
        e.update(cx, |e, cx| {
            e.focus(w, cx);
            e.set_selected_range(from..to, cx);
        });
    })
    .unwrap();
    cx.run_until_parked();
}
fn key(cx: &mut TestAppContext, o: &Opened, k: &str) {
    cx.update_window(o.win, |_, w, cx| w.press(k, cx)).unwrap();
    cx.run_until_parked();
}
fn click(cx: &mut TestAppContext, o: &Opened, id: &'static str) {
    cx.update_window(o.win, |_, w, cx| w.click(id, cx)).unwrap();
    cx.run_until_parked();
}
/// The SQL of each statement of the finished run.
fn ran(cx: &mut TestAppContext, o: &Opened) -> Vec<String> {
    o.idle(cx);
    o.model(cx).read_with(cx, |m, _| {
        m.results().iter().map(|r| r.sql.clone()).collect()
    })
}
fn text(cx: &mut TestAppContext, o: &Opened) -> String {
    o.shell
        .read_with(cx, |s, cx| s.tab().unwrap().read(cx).sql(cx))
}

#[gpui_kit::test]
fn ctrl_enter_runs_the_statement_under_the_caret_and_types_nothing(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    place(cx, &o, TWO, 20, 20);
    key(cx, &o, "ctrl-enter");
    assert_eq!(ran(cx, &o), ["SELECT 2 AS b;"]);
    assert_eq!(text(cx, &o), TWO);
}

#[gpui_kit::test]
fn ctrl_enter_runs_exactly_the_selection(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    place(cx, &o, TWO, 0, 13);
    key(cx, &o, "ctrl-enter");
    assert_eq!(ran(cx, &o), ["SELECT 1 AS a"]);
}

#[gpui_kit::test]
fn ctrl_shift_enter_runs_the_whole_buffer(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    place(cx, &o, TWO, 3, 3);
    key(cx, &o, "ctrl-shift-enter");
    assert_eq!(ran(cx, &o), ["SELECT 1 AS a;", "SELECT 2 AS b;"]);
    assert_eq!(text(cx, &o), TWO);
}

#[gpui_kit::test]
fn the_run_button_runs_at_the_caret_and_run_all_runs_everything(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    place(cx, &o, TWO, 3, 3);
    click(cx, &o, "run");
    assert_eq!(ran(cx, &o), ["SELECT 1 AS a;"]);
    click(cx, &o, "run-all");
    assert_eq!(ran(cx, &o), ["SELECT 1 AS a;", "SELECT 2 AS b;"]);
}

#[cfg(not(target_os = "macos"))]
#[gpui_kit::test]
fn the_run_chip_spells_its_binding(cx: &mut TestAppContext) {
    use support::label_of;
    let o = opened(cx, "", "", 0);
    assert_eq!(
        label_of(cx, o.win, "run-keys").as_deref(),
        Some("Ctrl \u{23CE}")
    );
}
