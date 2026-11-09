//! The in-cell editor, the Delete key and Ctrl+S in a browse tab's grid.
use crate::support::{Opened, label_of, launched, opened, present, wait_real};
use dbento_app::grid::staged_display;
use dbento_app::results_pane::ResultsPane;
use dbento_app::results_pane::types_into_cell;
use dbento_app::staged::RowEdit;
use dbento_core::{EditValue, RelationKind};
use gpui_kit::Keystroke;
use gpui_kit::component::table::TableSelection;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, Entity, Focusable as _, TestAppContext};

const T: &str = "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT, note TEXT); \
                 INSERT INTO t VALUES (1, 'b', 'x'), (2, 'a', 'y'); \
                 CREATE TABLE plain(a, b); INSERT INTO plain VALUES (1, 2);";
const T_SQL: &str = "SELECT *\nFROM main.t\nORDER BY id ASC";

fn browse(cx: &mut TestAppContext, table: &str, sql: &str, extra: &str) -> Opened {
    let o = launched(cx, T, extra);
    o.open_relation(cx, table, RelationKind::Table);
    o.wait_browse(cx, sql);
    o
}
fn pane(cx: &mut TestAppContext, o: &Opened) -> Entity<ResultsPane> {
    o.shell
        .read_with(cx, |s, cx| s.tab().unwrap().read(cx).results_pane().clone())
}
/// Selects grid cell (row, data column) and focuses the grid.
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
fn type_text(cx: &mut TestAppContext, o: &Opened, text: &str) {
    cx.update_window(o.win, |_, w, cx| w.input(text, cx))
        .unwrap();
    cx.run_until_parked();
}
fn double_click(cx: &mut TestAppContext, o: &Opened, id: &'static str) {
    cx.update_window(o.win, |_, w, cx| w.double_click(id, cx))
        .unwrap();
    cx.run_until_parked();
}
fn editing(cx: &mut TestAppContext, o: &Opened) -> Option<(u64, usize)> {
    pane(cx, o).read_with(cx, |p, _| p.editing())
}
fn editor_text(cx: &mut TestAppContext, o: &Opened) -> String {
    pane(cx, o).read_with(cx, |p, cx| p.cell_editor().read(cx).value().to_string())
}
fn staged(cx: &mut TestAppContext, o: &Opened, row: u64, col: usize) -> Option<EditValue> {
    o.model(cx)
        .read_with(cx, |m, _| m.edits().cell(row, col).cloned())
}

#[test]
fn staged_values_display_like_cells() {
    assert_eq!(
        staged_display(&EditValue::Text("a\nb".into())),
        ("a".into(), false)
    );
    assert_eq!(staged_display(&EditValue::Null), ("NULL".into(), true));
    assert_eq!(
        staged_display(&EditValue::Default),
        ("DEFAULT".into(), true)
    );
}

#[gpui_kit::test]
fn double_click_opens_the_editor_and_enter_stages(cx: &mut TestAppContext) {
    let o = browse(cx, "t", T_SQL, "");
    double_click(cx, &o, "cell-0-1");
    assert_eq!(editing(cx, &o), Some((0, 1)));
    assert!(present(cx, o.win, "cell-editor"));
    assert_eq!(editor_text(cx, &o), "b");
    type_text(cx, &o, "z");
    assert_eq!(
        editor_text(cx, &o),
        "z",
        "the opened text is selected, so typing replaces it"
    );
    press(cx, &o, "enter");
    assert_eq!(editing(cx, &o), None);
    assert!(!present(cx, o.win, "cell-editor"));
    assert_eq!(staged(cx, &o, 0, 1), Some(EditValue::Text("z".into())));
    assert_eq!(label_of(cx, o.win, "cell-0-1").as_deref(), Some("z"));
    assert_eq!(o.fx.count("SELECT count(*) FROM t WHERE name = 'z'"), 0);
}

#[gpui_kit::test]
fn enter_and_f2_open_the_selected_cell_and_escape_cancels(cx: &mut TestAppContext) {
    let o = browse(cx, "t", T_SQL, "");
    select(cx, &o, 1, 1);
    press(cx, &o, "enter");
    assert_eq!(editing(cx, &o), Some((1, 1)));
    type_text(cx, &o, "q");
    press(cx, &o, "escape");
    assert_eq!(editing(cx, &o), None);
    assert_eq!(staged(cx, &o, 1, 1), None);
    assert_eq!(label_of(cx, o.win, "cell-1-1").as_deref(), Some("a"));
    select(cx, &o, 1, 1);
    press(cx, &o, "f2");
    assert_eq!(editing(cx, &o), Some((1, 1)));
    assert_eq!(editor_text(cx, &o), "a");
}

// Real platforms send Enter, Tab, Escape, Backspace and Delete with a control-character
// key_char; the test window does not, so the predicate is pinned directly.
#[test]
fn only_printable_unmodified_keys_type_into_a_cell() {
    let key = |s: &'static str, ch: Option<&str>| {
        let mut k = Keystroke::parse(s).unwrap();
        k.key_char = ch.map(str::to_owned);
        k
    };
    assert!(types_into_cell(&key("q", Some("q"))));
    assert!(types_into_cell(&key("shift-q", Some("Q"))));
    assert!(types_into_cell(&key("space", Some(" "))));
    assert!(types_into_cell(&key("e", Some("\u{e9}"))));
    assert!(!types_into_cell(&key("q", None)));
    assert!(!types_into_cell(&key("ctrl-q", Some("q"))));
    assert!(!types_into_cell(&key("alt-q", Some("\u{153}"))));
    for (k, c) in [
        ("escape", "\u{1b}"),
        ("tab", "\t"),
        ("enter", "\r"),
        ("backspace", "\u{8}"),
        ("delete", "\u{7f}"),
    ] {
        assert!(!types_into_cell(&key(k, Some(c))), "{k}");
    }
}

#[gpui_kit::test]
fn tab_stages_and_opens_the_next_cell(cx: &mut TestAppContext) {
    let o = browse(cx, "t", T_SQL, "");
    select(cx, &o, 0, 1);
    press(cx, &o, "enter");
    type_text(cx, &o, "w");
    press(cx, &o, "tab");
    assert_eq!(staged(cx, &o, 0, 1), Some(EditValue::Text("w".into())));
    assert_eq!(editing(cx, &o), Some((0, 2)));
    assert_eq!(editor_text(cx, &o), "x");
    press(cx, &o, "tab");
    assert_eq!(editing(cx, &o), None, "the last column closes the editor");
    let sel = pane(cx, &o).read_with(cx, |p, cx| p.table().read(cx).selection());
    assert_eq!(sel, TableSelection::Cell(0, 3));
}

#[gpui_kit::test]
fn typing_on_a_selected_cell_opens_the_editor_with_that_text(cx: &mut TestAppContext) {
    let o = browse(cx, "t", T_SQL, "");
    select(cx, &o, 1, 1);
    type_text(cx, &o, "qr");
    assert_eq!(editing(cx, &o), Some((1, 1)));
    assert_eq!(editor_text(cx, &o), "qr");
    press(cx, &o, "enter");
    assert_eq!(staged(cx, &o, 1, 1), Some(EditValue::Text("qr".into())));
}

#[gpui_kit::test]
fn a_capital_counts_but_a_chord_does_not(cx: &mut TestAppContext) {
    let o = browse(cx, "t", T_SQL, "");
    select(cx, &o, 0, 2);
    press(cx, &o, "alt-q");
    assert_eq!(editing(cx, &o), None);
    type_text(cx, &o, "Q");
    assert_eq!(editing(cx, &o), Some((0, 2)));
    assert_eq!(editor_text(cx, &o), "Q");
}

#[gpui_kit::test]
fn typing_on_a_query_result_shows_the_read_only_note(cx: &mut TestAppContext) {
    let o = opened(cx, T, "", 0);
    o.run(cx, "SELECT * FROM t");
    o.idle(cx);
    select(cx, &o, 0, 1);
    type_text(cx, &o, "z");
    assert_eq!(editing(cx, &o), None);
    assert_eq!(
        label_of(cx, o.win, "readonly-note").as_deref(),
        Some("Query results are read-only.")
    );
}

#[gpui_kit::test]
fn selecting_another_cell_stages_the_open_editor(cx: &mut TestAppContext) {
    let o = browse(cx, "t", T_SQL, "");
    double_click(cx, &o, "cell-0-2");
    type_text(cx, &o, "n");
    cx.update_window(o.win, |_, w, cx| w.click("cell-1-1", cx))
        .unwrap();
    cx.run_until_parked();
    assert_eq!(editing(cx, &o), None);
    assert_eq!(staged(cx, &o, 0, 2), Some(EditValue::Text("n".into())));
}

#[gpui_kit::test]
fn clicking_inside_the_editor_keeps_it_open(cx: &mut TestAppContext) {
    let o = browse(cx, "t", T_SQL, "");
    double_click(cx, &o, "cell-0-1");
    type_text(cx, &o, "n");
    cx.update_window(o.win, |_, w, cx| w.click("cell-editor", cx))
        .unwrap();
    cx.run_until_parked();
    assert_eq!(editing(cx, &o), Some((0, 1)));
    assert_eq!(editor_text(cx, &o), "n");
    assert_eq!(staged(cx, &o, 0, 1), None);
}

#[gpui_kit::test]
fn enter_on_a_row_selection_opens_nothing(cx: &mut TestAppContext) {
    let o = browse(cx, "t", T_SQL, "");
    let table = pane(cx, &o).read_with(cx, |p, _| p.table().clone());
    cx.update_window(o.win, |_, w, cx| {
        table.update(cx, |t, cx| t.set_selected_row(0, cx));
        table.read(cx).focus_handle(cx).focus(w, cx);
    })
    .unwrap();
    cx.run_until_parked();
    press(cx, &o, "enter");
    press(cx, &o, "f2");
    assert_eq!(editing(cx, &o), None);
}

#[gpui_kit::test]
fn ctrl_s_in_the_filter_input_saves_the_edits(cx: &mut TestAppContext) {
    let o = browse(cx, "t", T_SQL, "");
    let m = o.model(cx);
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 1, "z", cx)));
    let input = pane(cx, &o).read_with(cx, |p, _| p.filter_input().clone());
    cx.update_window(o.win, |_, w, cx| input.update(cx, |s, cx| s.focus(w, cx)))
        .unwrap();
    press(cx, &o, "ctrl-s");
    wait_real(cx, "the apply prompt", |cx| {
        m.read_with(cx, |m, _| m.apply_prompt().is_some())
    });
    m.update(cx, |m, cx| m.rollback(cx));
    assert!(!present(cx, o.win, "save-name"));
}

#[gpui_kit::test]
fn delete_marks_the_selected_row(cx: &mut TestAppContext) {
    let o = browse(cx, "t", T_SQL, "");
    select(cx, &o, 1, 0);
    press(cx, &o, "delete");
    let row = o.model(cx).read_with(cx, |m, _| m.edits().row(1).cloned());
    assert!(matches!(row, Some(RowEdit::Delete { .. })), "{row:?}");
    assert_eq!(o.fx.count("SELECT count(*) FROM t"), 2);
}

#[gpui_kit::test]
fn delete_while_typing_edits_the_text(cx: &mut TestAppContext) {
    let o = browse(cx, "t", T_SQL, "");
    double_click(cx, &o, "cell-0-1");
    press(cx, &o, "delete");
    assert_eq!(editing(cx, &o), Some((0, 1)));
    assert_eq!(editor_text(cx, &o), "");
    assert!(o.model(cx).read_with(cx, |m, _| m.edits().row(0).is_none()));
}

#[gpui_kit::test]
fn a_locator_cell_does_not_open(cx: &mut TestAppContext) {
    let o = browse(
        cx,
        "plain",
        "SELECT rowid, *\nFROM main.plain\nORDER BY rowid ASC",
        "",
    );
    double_click(cx, &o, "cell-0-0");
    assert_eq!(editing(cx, &o), None);
    double_click(cx, &o, "cell-0-1");
    assert_eq!(editing(cx, &o), Some((0, 1)));
}

#[gpui_kit::test]
fn a_read_only_browse_does_not_open(cx: &mut TestAppContext) {
    let o = browse(cx, "t", T_SQL, "read_only = true");
    double_click(cx, &o, "cell-0-1");
    assert_eq!(editing(cx, &o), None);
    select(cx, &o, 0, 1);
    press(cx, &o, "delete");
    assert!(o.model(cx).read_with(cx, |m, _| m.edits().is_empty()));
}

#[gpui_kit::test]
fn a_free_query_result_does_not_open(cx: &mut TestAppContext) {
    let o = opened(cx, T, "", 0);
    o.run(cx, "SELECT * FROM t");
    o.idle(cx);
    double_click(cx, &o, "cell-0-1");
    assert_eq!(editing(cx, &o), None);
}

#[gpui_kit::test]
fn ctrl_s_in_the_grid_saves_the_edits(cx: &mut TestAppContext) {
    let o = browse(cx, "t", T_SQL, "");
    double_click(cx, &o, "cell-0-1");
    type_text(cx, &o, "z");
    press(cx, &o, "ctrl-s");
    let m = o.model(cx);
    wait_real(cx, "the apply prompt", |cx| {
        m.read_with(cx, |m, _| m.apply_prompt().is_some())
    });
    assert_eq!(staged(cx, &o, 0, 1), Some(EditValue::Text("z".into())));
    m.update(cx, |m, cx| m.rollback(cx));
    assert!(!present(cx, o.win, "save-name"));
}

#[gpui_kit::test]
fn ctrl_s_with_nothing_staged_still_saves_the_query(cx: &mut TestAppContext) {
    let o = opened(cx, T, "", 0);
    o.run(cx, "SELECT * FROM t");
    o.idle(cx);
    select(cx, &o, 0, 0);
    press(cx, &o, "ctrl-s");
    wait_real(cx, "the name dialog", |cx| present(cx, o.win, "save-name"));
}

#[gpui_kit::test]
fn a_rerun_while_editing_hands_the_keys_back_to_the_grid(cx: &mut TestAppContext) {
    let o = browse(cx, "t", T_SQL, "");
    double_click(cx, &o, "cell-0-1");
    let m = o.model(cx);
    m.update(cx, |m, cx| m.apply_filter("id > 0", cx));
    o.wait_browse(
        cx,
        "SELECT *\nFROM main.t\nWHERE (\nid > 0\n)\nORDER BY id ASC",
    );
    assert_eq!(editing(cx, &o), None);
    // Selected without focusing the grid by hand: the keys must reach it on their own.
    let table = pane(cx, &o).read_with(cx, |p, _| p.table().clone());
    table.update(cx, |t, cx| t.set_selected_cell(1, 1, cx));
    cx.run_until_parked();
    press(cx, &o, "delete");
    let row = m.read_with(cx, |m, _| m.edits().row(1).cloned());
    assert!(matches!(row, Some(RowEdit::Delete { .. })), "{row:?}");
}
