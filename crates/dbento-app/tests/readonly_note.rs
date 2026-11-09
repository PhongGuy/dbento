//! An edit attempt on a free query result says why nothing opens and offers the table.
use crate::support::{Opened, label_of, launched, opened, present, wait_real};
use dbento_app::results_pane::ResultsPane;
use dbento_core::RelationKind;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, Entity, Focusable as _, TestAppContext};

const T: &str = "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT); \
                 INSERT INTO t VALUES (1, 'a'), (2, 'b'); \
                 CREATE TABLE u(id INTEGER PRIMARY KEY, t_id INTEGER); \
                 INSERT INTO u VALUES (1, 1); \
                 CREATE VIEW v AS SELECT * FROM t;";
const NOTE: &str = "Query results are read-only.";

fn pane(cx: &mut TestAppContext, o: &Opened) -> Entity<ResultsPane> {
    o.shell
        .read_with(cx, |s, cx| s.tab().unwrap().read(cx).results_pane().clone())
}
fn select(cx: &mut TestAppContext, o: &Opened, row: usize, data: usize) {
    let table = pane(cx, o).read_with(cx, |p, _| p.table().clone());
    cx.update_window(o.win, |_, w, cx| {
        table.update(cx, |t, cx| t.set_selected_cell(row, data + 1, cx));
        table.read(cx).focus_handle(cx).focus(w, cx);
    })
    .unwrap();
    cx.run_until_parked();
}
fn press(cx: &mut TestAppContext, o: &Opened, key: &str) {
    cx.update_window(o.win, |_, w, cx| w.press(key, cx))
        .unwrap();
    cx.run_until_parked();
}
fn double_click(cx: &mut TestAppContext, o: &Opened, id: &'static str) {
    cx.update_window(o.win, |_, w, cx| w.double_click(id, cx))
        .unwrap();
    cx.run_until_parked();
}
/// Waits for the note's button to read `want`; it waits on the explorer's catalog.
fn wait_open(cx: &mut TestAppContext, o: &Opened, want: &str) {
    wait_real(cx, want, |cx| {
        label_of(cx, o.win, "readonly-open").as_deref() == Some(want)
    });
}
fn queried(cx: &mut TestAppContext, sql: &str) -> Opened {
    let o = opened(cx, T, "", 0);
    o.run(cx, sql);
    o.idle(cx);
    o
}

#[gpui_kit::test]
fn double_click_explains_and_opens_the_table(cx: &mut TestAppContext) {
    let o = queried(cx, "SELECT * FROM t");
    assert!(!present(cx, o.win, "readonly-note"));
    double_click(cx, &o, "cell-0-1");
    assert_eq!(label_of(cx, o.win, "readonly-note").as_deref(), Some(NOTE));
    wait_open(cx, &o, "Open t to edit");
    cx.update_window(o.win, |_, w, cx| w.click("readonly-open", cx))
        .unwrap();
    cx.run_until_parked();
    o.wait_browse(cx, "SELECT *\nFROM main.t\nORDER BY id ASC");
}

// SQLite keeps an unquoted name's case in the query while the table's own name differs;
// the catalog's name is the one offered and opened.
#[gpui_kit::test]
fn a_sqlite_name_in_another_case_opens_the_catalog_name(cx: &mut TestAppContext) {
    let o = queried(cx, "SELECT * FROM T");
    double_click(cx, &o, "cell-0-1");
    wait_open(cx, &o, "Open t to edit");
    cx.update_window(o.win, |_, w, cx| w.click("readonly-open", cx))
        .unwrap();
    cx.run_until_parked();
    o.wait_browse(cx, "SELECT *\nFROM main.t\nORDER BY id ASC");
}

// A view cannot be edited in a browse tab either, so its note offers nothing to open.
#[gpui_kit::test]
fn a_view_gets_the_note_without_the_button(cx: &mut TestAppContext) {
    let o = queried(cx, "SELECT * FROM v");
    double_click(cx, &o, "cell-0-1");
    assert_eq!(label_of(cx, o.win, "readonly-note").as_deref(), Some(NOTE));
    let sidebar = o.shell.read_with(cx, |s, _| s.sidebar().cloned().unwrap());
    wait_real(cx, "the catalog's relations", |cx| {
        sidebar.read_with(cx, |s, _| s.tree().relations("lite", "main").is_some())
    });
    assert!(!present(cx, o.win, "readonly-open"));
}

#[gpui_kit::test]
fn a_sqlite_name_in_another_case_reuses_the_open_browse_tab(cx: &mut TestAppContext) {
    let o = launched(cx, T, "");
    o.open_relation(cx, "t", RelationKind::Table);
    o.wait_browse(cx, "SELECT *\nFROM main.t\nORDER BY id ASC");
    cx.update_window(o.win, |_, w, cx| {
        o.shell.update(cx, |s, cx| s.new_tab(w, cx))
    })
    .unwrap();
    o.run(cx, "SELECT * FROM T");
    o.idle(cx);
    double_click(cx, &o, "cell-0-1");
    wait_open(cx, &o, "Open t to edit");
    cx.update_window(o.win, |_, w, cx| w.click("readonly-open", cx))
        .unwrap();
    cx.run_until_parked();
    o.wait_browse(cx, "SELECT *\nFROM main.t\nORDER BY id ASC");
    assert_eq!(o.shell.read_with(cx, |s, _| s.tabs().len()), 2);
}

#[gpui_kit::test]
fn a_join_gets_the_note_without_the_button(cx: &mut TestAppContext) {
    let o = queried(cx, "SELECT t.name, u.id FROM t JOIN u ON u.t_id = t.id");
    double_click(cx, &o, "cell-0-0");
    assert_eq!(label_of(cx, o.win, "readonly-note").as_deref(), Some(NOTE));
    assert!(!present(cx, o.win, "readonly-open"));
}

#[gpui_kit::test]
fn enter_shows_it_and_escape_or_a_run_hides_it(cx: &mut TestAppContext) {
    let o = queried(cx, "SELECT * FROM t");
    select(cx, &o, 0, 1);
    press(cx, &o, "enter");
    assert!(present(cx, o.win, "readonly-note"));
    press(cx, &o, "escape");
    assert!(!present(cx, o.win, "readonly-note"));
    select(cx, &o, 1, 1);
    press(cx, &o, "f2");
    assert!(present(cx, o.win, "readonly-note"));
    o.run(cx, "SELECT * FROM t");
    o.idle(cx);
    assert!(!present(cx, o.win, "readonly-note"));
}

#[gpui_kit::test]
fn a_browse_tab_never_shows_it(cx: &mut TestAppContext) {
    let o = launched(cx, T, "read_only = true");
    o.open_relation(cx, "t", RelationKind::Table);
    o.wait_browse(cx, "SELECT *\nFROM main.t\nORDER BY id ASC");
    double_click(cx, &o, "cell-0-1");
    assert!(!present(cx, o.win, "readonly-note"));
}
