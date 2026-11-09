//! Opening a plan: keys, toolbar, palette entries and the ANALYZE question's wording.
use crate::support::{Opened, label_of, open_connection_row, opened, present, started, wait_real};
use dbento_app::palette::PaletteCommand;
use dbento_app::query_tab::analyze_ask;
use dbento_core::Unguarded;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext};

fn place(cx: &mut TestAppContext, o: &Opened, sql: &str) {
    let tab = o.shell.read_with(cx, |s, _| s.tab().unwrap().clone());
    let e = o
        .shell
        .read_with(cx, |s, cx| s.tab().unwrap().read(cx).editor().clone());
    cx.update_window(o.win, |_, w, cx| {
        tab.update(cx, |t, cx| t.set_sql(sql, w, cx));
        e.update(cx, |e, cx| {
            e.focus(w, cx);
            e.set_selected_range(0..0, cx);
        });
    })
    .unwrap();
    cx.run_until_parked();
}
fn key(cx: &mut TestAppContext, o: &Opened, k: &str) {
    cx.update_window(o.win, |_, w, cx| w.press(k, cx)).unwrap();
    cx.run_until_parked();
}

#[test]
fn the_analyze_ask_says_it_runs_and_rolls_back() {
    let a = analyze_ask(
        &[Unguarded::DeleteWithoutWhere, Unguarded::Drop],
        "prod",
        "DELETE FROM t",
    );
    assert_eq!(
        a.title.as_ref(),
        "EXPLAIN ANALYZE runs this statement on 'prod'"
    );
    let d = a.detail.clone().unwrap();
    assert!(d.starts_with("ANALYZE executes the statement to time it. Its changes are rolled back, but sequence values it takes and effects outside the transaction are kept."), "{d}");
    assert!(
        d.ends_with("It is also: DELETE without WHERE, DROP."),
        "{d}"
    );
    assert_eq!(a.sql.as_deref(), Some("DELETE FROM t"));
    let choices: Vec<_> = a
        .choices
        .iter()
        .map(|c| (c.id, c.label.to_string(), c.danger))
        .collect();
    assert_eq!(
        choices,
        [
            ("confirm-skip", "Cancel".to_owned(), false),
            ("confirm-run", "Run ANALYZE".to_owned(), true)
        ]
    );
    assert_eq!((a.default, a.cancel), (0, 0));
    assert!(
        !analyze_ask(&[], "dev", "INSERT INTO t VALUES (1)")
            .detail
            .unwrap()
            .contains("It is also")
    );
}

#[test]
fn the_palette_lists_both_commands_after_format() {
    let all = PaletteCommand::ALL;
    let at = all
        .iter()
        .position(|c| *c == PaletteCommand::FormatSql)
        .unwrap();
    assert_eq!(
        (all[at + 1].label(), all[at + 2].label()),
        ("Explain plan", "Explain analyze")
    );
}

#[gpui_kit::test]
fn ctrl_e_plans_the_statement_at_the_caret(cx: &mut TestAppContext) {
    let o = opened(cx, "CREATE TABLE t(x); INSERT INTO t VALUES (1);", "", 0);
    place(cx, &o, "SELECT * FROM t WHERE x > 0;\nSELECT 2");
    key(cx, &o, "ctrl-e");
    o.idle(cx);
    assert!(present(cx, o.win, "plan-tree"));
    let m = o.model(cx);
    m.read_with(cx, |m, _| {
        assert_eq!(m.results()[0].sql, "SELECT * FROM t WHERE x > 0;");
        assert_eq!(m.results()[0].source, Some(0..28));
    });
}

#[gpui_kit::test]
fn ctrl_shift_e_on_sqlite_never_asks_and_notes_the_missing_timings(cx: &mut TestAppContext) {
    let o = opened(cx, "CREATE TABLE t(x); INSERT INTO t VALUES (1);", "", 0);
    place(cx, &o, "DELETE FROM t WHERE x > 0");
    key(cx, &o, "ctrl-shift-e");
    o.idle(cx);
    assert!(!present(cx, o.win, "confirm-run"));
    assert!(present(cx, o.win, "plan-note"));
    assert_eq!(o.fx.count("SELECT count(*) FROM t"), 1);
}

#[gpui_kit::test]
fn an_explain_statement_is_refused_and_nothing_runs(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let m = o.model(cx);
    let before = m.read_with(cx, |m, _| m.run_id());
    place(cx, &o, "EXPLAIN SELECT 1");
    key(cx, &o, "ctrl-e");
    m.read_with(cx, |m, _| {
        assert_eq!(m.run_id(), before);
        assert!(m.results().is_empty());
    });
}

#[gpui_kit::test]
fn the_toolbar_button_explains_too(cx: &mut TestAppContext) {
    let o = opened(cx, "CREATE TABLE t(x);", "", 0);
    place(cx, &o, "SELECT * FROM t");
    cx.update_window(o.win, |_, w, cx| w.click("explain", cx))
        .unwrap();
    o.idle(cx);
    assert!(present(cx, o.win, "plan-view"));
}

/// A MySQL connection that never connects; the tab opens and its preconnect fails quietly.
const MY_CLOSED: &str = "dbento = 1\n[connections.my]\nengine = \"mysql\"\nhost = \"127.0.0.1\"\nport = 9\nuser = \"u\"\ndatabase = \"d\"\n";

#[gpui_kit::test]
fn mysql_hides_explain_but_its_key_still_says_why(cx: &mut TestAppContext) {
    let s = started(cx, MY_CLOSED);
    open_connection_row(cx, s.win, "my");
    assert!(present(cx, s.win, "format"));
    assert!(!present(cx, s.win, "explain"));
    assert!(!present(cx, s.win, "explain-tip"));

    let tab = s.shell.read_with(cx, |sh, _| sh.tab().unwrap().clone());
    let e = s
        .shell
        .read_with(cx, |sh, cx| sh.tab().unwrap().read(cx).editor().clone());
    cx.update_window(s.win, |_, w, cx| {
        tab.update(cx, |t, cx| t.set_sql("SELECT 1", w, cx));
        e.update(cx, |e, cx| {
            e.focus(w, cx);
            e.set_selected_range(0..0, cx);
        });
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(s.win, |_, w, cx| w.press("ctrl-e", cx))
        .unwrap();
    wait_real(cx, "the unsupported notice", |cx| {
        label_of(cx, s.win, "notice").as_deref()
            == Some("EXPLAIN is not available on this engine yet")
    });

    cx.update_window(s.win, |_, w, cx| w.right_click("editor", cx))
        .unwrap();
    cx.run_until_parked();
    assert!(present(cx, s.win, "editor-menu-run"));
    assert!(!present(cx, s.win, "editor-menu-explain"));
    assert!(!present(cx, s.win, "editor-menu-explain-analyze"));
}
