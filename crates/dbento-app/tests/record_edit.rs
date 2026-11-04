//! Editing a field in the record form: multi-line text, bytes as hex, cancel, implicit staging.
mod support;
use dbento_app::record_form::RecordForm;
use dbento_core::{EditValue, RelationKind};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, ElementId, Entity, Focusable as _, TestAppContext};
use support::{Opened, label_of, launched, opened, present, wait_real};

const T: &str = "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT, note TEXT, pic BLOB); \
                 INSERT INTO t VALUES (1, 'b', 'two\nlines', x'01'), (2, 'a', NULL, NULL); \
                 CREATE TABLE np(a TEXT); INSERT INTO np VALUES ('x');";

fn record(cx: &mut TestAppContext, table: &str, sql: &str) -> Opened {
    record_on(cx, table, sql, "")
}
fn record_on(cx: &mut TestAppContext, table: &str, sql: &str, extra: &str) -> Opened {
    let o = launched(cx, T, extra);
    o.open_relation(cx, table, RelationKind::Table);
    o.wait_browse(cx, sql);
    click(cx, &o, "row-view-expand");
    o
}
fn form(cx: &mut TestAppContext, o: &Opened) -> Entity<RecordForm> {
    o.shell.read_with(cx, |s, cx| {
        s.tab()
            .unwrap()
            .read(cx)
            .results_pane()
            .read(cx)
            .record_form()
            .clone()
    })
}
fn click(cx: &mut TestAppContext, o: &Opened, id: impl Into<ElementId>) {
    let id = id.into();
    cx.update_window(o.win, |_, w, cx| w.click(id, cx)).unwrap();
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
fn staged(cx: &mut TestAppContext, o: &Opened, row: u64, col: usize) -> Option<EditValue> {
    o.model(cx)
        .read_with(cx, |m, _| m.edits().cell(row, col).cloned())
}
const T_SQL: &str = "SELECT *\nFROM main.t\nORDER BY id ASC";

#[gpui_kit::test]
fn ctrl_enter_stages_a_multi_line_value_and_runs_nothing(cx: &mut TestAppContext) {
    let o = record(cx, "t", T_SQL);
    let run_id = o.model(cx).read_with(cx, |m, _| m.run_id());
    click(cx, &o, ("record-value", 2usize));
    assert!(present(cx, o.win, "record-editor"));
    let f = form(cx, &o);
    assert_eq!(
        f.read_with(cx, |f, cx| f.editor().read(cx).value().to_string()),
        "two\nlines"
    );
    type_text(cx, &o, "x");
    press(cx, &o, "enter");
    type_text(cx, &o, "y");
    press(cx, &o, "ctrl-enter");
    assert!(!present(cx, o.win, "record-editor"));
    assert_eq!(staged(cx, &o, 0, 2), Some(EditValue::Text("x\ny".into())));
    assert!(present(cx, o.win, ("record-modified", 2usize)));
    assert_eq!(o.model(cx).read_with(cx, |m, _| m.run_id()), run_id);
    let m = o.model(cx);
    assert!(m.update(cx, |m, cx| m.undo(cx)));
    assert_eq!(staged(cx, &o, 0, 2), None);
}

#[gpui_kit::test]
fn escape_cancels_and_another_field_or_row_stages_the_open_one(cx: &mut TestAppContext) {
    let o = record(cx, "t", T_SQL);
    click(cx, &o, ("record-value", 1usize));
    type_text(cx, &o, "zz");
    press(cx, &o, "escape");
    assert!(!present(cx, o.win, "record-editor"));
    assert_eq!(staged(cx, &o, 0, 1), None);
    click(cx, &o, ("record-value", 1usize));
    type_text(cx, &o, "q");
    click(cx, &o, ("record-value", 2usize));
    assert_eq!(staged(cx, &o, 0, 1), Some(EditValue::Text("q".into())));
    assert_eq!(form(cx, &o).read_with(cx, |f, _| f.editing()), Some(2));
    type_text(cx, &o, "w");
    click(cx, &o, "record-next");
    assert_eq!(staged(cx, &o, 0, 2), Some(EditValue::Text("w".into())));
    assert_eq!(form(cx, &o).read_with(cx, |f, _| f.editing()), None);
    assert_eq!(
        label_of(cx, o.win, "record-position").as_deref(),
        Some("Row 2 of 2")
    );
}

#[gpui_kit::test]
fn bytes_edit_as_hex_and_bad_hex_stays_open_with_an_error(cx: &mut TestAppContext) {
    let o = record(cx, "t", T_SQL);
    click(cx, &o, ("record-value", 3usize));
    let f = form(cx, &o);
    assert_eq!(
        f.read_with(cx, |f, cx| f.editor().read(cx).value().to_string()),
        "\\x01"
    );
    type_text(cx, &o, "\\x0a0b");
    press(cx, &o, "ctrl-enter");
    assert_eq!(staged(cx, &o, 0, 3), Some(EditValue::Bytes(vec![10, 11])));
    click(cx, &o, "record-next");
    click(cx, &o, ("record-value", 3usize));
    type_text(cx, &o, "zz");
    press(cx, &o, "ctrl-enter");
    assert_eq!(
        label_of(cx, o.win, "record-error").as_deref(),
        Some("bytes are written as \\x and hex digits")
    );
    assert!(present(cx, o.win, "record-editor"));
    assert_eq!(staged(cx, &o, 1, 3), None);
}

#[gpui_kit::test]
fn a_locator_is_marked_and_never_opens(cx: &mut TestAppContext) {
    let o = record(
        cx,
        "np",
        "SELECT rowid, *\nFROM main.np\nORDER BY rowid ASC",
    );
    assert!(present(cx, o.win, ("record-lock", 0usize)));
    click(cx, &o, ("record-value", 0usize));
    assert!(!present(cx, o.win, "record-editor"));
    click(cx, &o, ("record-value", 1usize));
    assert!(present(cx, o.win, "record-editor"));
}

#[gpui_kit::test]
fn switching_to_the_grid_stages_the_open_field(cx: &mut TestAppContext) {
    let o = record(cx, "t", T_SQL);
    click(cx, &o, ("record-value", 1usize));
    type_text(cx, &o, "g");
    click(cx, &o, "row-view-expand");
    assert_eq!(staged(cx, &o, 0, 1), Some(EditValue::Text("g".into())));
    assert_eq!(form(cx, &o).read_with(cx, |f, _| f.editing()), None);
}

#[gpui_kit::test]
fn an_inserted_row_count_change_drops_the_open_field_unstaged(cx: &mut TestAppContext) {
    let o = record(cx, "t", T_SQL);
    let m = o.model(cx);
    assert!(m.update(cx, |m, cx| m.add_row(cx)).is_some());
    // The view stays on loaded row 0, now table row 1, under the new row.
    click(cx, &o, ("record-value", 1usize));
    type_text(cx, &o, "q");
    click(cx, &o, "revert-edits");
    assert_eq!(form(cx, &o).read_with(cx, |f, _| f.editing()), None);
    assert!(!present(cx, o.win, "record-editor"));
    assert_eq!(staged(cx, &o, 0, 1), None);
    assert_eq!(staged(cx, &o, 1, 1), None);
}

#[gpui_kit::test]
fn a_rerun_drops_the_open_field_unstaged(cx: &mut TestAppContext) {
    let o = record(cx, "t", T_SQL);
    click(cx, &o, ("record-value", 1usize));
    type_text(cx, &o, "q");
    let run_id = o.model(cx).read_with(cx, |m, _| m.run_id());
    o.run(cx, T_SQL);
    o.idle(cx);
    assert_ne!(o.model(cx).read_with(cx, |m, _| m.run_id()), run_id);
    assert_eq!(form(cx, &o).read_with(cx, |f, _| f.editing()), None);
    assert!(!present(cx, o.win, "record-editor"));
    assert_eq!(staged(cx, &o, 0, 1), None);
}

#[gpui_kit::test]
fn ctrl_s_stages_the_open_field_and_saves(cx: &mut TestAppContext) {
    let o = record(cx, "t", T_SQL);
    click(cx, &o, ("record-value", 1usize));
    type_text(cx, &o, "q");
    press(cx, &o, "ctrl-s");
    assert_eq!(staged(cx, &o, 0, 1), Some(EditValue::Text("q".into())));
    assert_eq!(form(cx, &o).read_with(cx, |f, _| f.editing()), None);
    let m = o.model(cx);
    wait_real(cx, "the apply prompt", |cx| {
        m.read_with(cx, |m, _| m.apply_prompt().is_some())
    });
    m.update(cx, |m, cx| m.rollback(cx));
}

#[gpui_kit::test]
fn the_save_button_stages_the_open_field(cx: &mut TestAppContext) {
    let o = record(cx, "t", T_SQL);
    let m = o.model(cx);
    assert!(m.update(cx, |m, cx| m.stage_cell(1, 1, "z", cx)));
    click(cx, &o, ("record-value", 1usize));
    type_text(cx, &o, "q");
    click(cx, &o, "save-edits");
    assert_eq!(staged(cx, &o, 0, 1), Some(EditValue::Text("q".into())));
    wait_real(cx, "the apply prompt", |cx| {
        m.read_with(cx, |m, _| m.apply_prompt().is_some())
    });
    m.update(cx, |m, cx| m.rollback(cx));
}

#[gpui_kit::test]
fn a_free_result_never_opens_the_editor(cx: &mut TestAppContext) {
    let o = opened(cx, T, "", 0);
    o.run(cx, "SELECT 1 AS n");
    o.idle(cx);
    click(cx, &o, "row-view-expand");
    click(cx, &o, ("record-value", 0usize));
    assert!(!present(cx, o.win, "record-editor"));
}

#[gpui_kit::test]
fn a_read_only_tab_never_opens_the_editor(cx: &mut TestAppContext) {
    let o = record_on(cx, "t", T_SQL, "read_only = true");
    click(cx, &o, ("record-value", 1usize));
    assert!(!present(cx, o.win, "record-editor"));
}

#[gpui_kit::test]
fn a_rerun_dropping_the_open_field_leaves_the_sql_editor_focused(cx: &mut TestAppContext) {
    let o = record(cx, "t", T_SQL);
    click(cx, &o, ("record-value", 1usize));
    let editor = o
        .shell
        .read_with(cx, |s, cx| s.tab().unwrap().read(cx).editor().clone());
    cx.update_window(o.win, |_, w, cx| {
        editor.read(cx).focus_handle(cx).focus(w, cx)
    })
    .unwrap();
    o.run(cx, T_SQL);
    o.idle(cx);
    assert_eq!(form(cx, &o).read_with(cx, |f, _| f.editing()), None);
    cx.update_window(o.win, |_, w, cx| {
        assert!(editor.read(cx).focus_handle(cx).is_focused(w));
    })
    .unwrap();
}
