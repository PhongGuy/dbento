//! Enter in the record form: stages a one-line value, keeps newlines where text is long.
mod support;
use dbento_app::record_form::{edit_hint, enter_inserts_newline};
use dbento_app::results_pane::ResultsPane;
use dbento_core::{EditValue, RelationKind};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, ElementId, Entity, Focusable as _, TestAppContext};
use support::{Opened, click_id, label_of, launched, present};

const T: &str = "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT, doc JSON, x XML); \
                 INSERT INTO t VALUES (1, 'b', '{}', '<a/>'), (2, 'a', NULL, NULL);";

fn browse(cx: &mut TestAppContext) -> Opened {
    let o = launched(cx, T, "");
    o.open_relation(cx, "t", RelationKind::Table);
    o.wait_browse(cx, "SELECT *\nFROM main.t\nORDER BY id ASC");
    o
}
fn record(cx: &mut TestAppContext) -> Opened {
    let o = browse(cx);
    click(cx, &o, "row-view-expand");
    o
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

#[test]
fn long_text_types_and_values_with_newlines_keep_enter_as_a_newline() {
    assert!(enter_inserts_newline("JSONB", "{}"));
    assert!(enter_inserts_newline("json", ""));
    assert!(enter_inserts_newline("XML", "<a/>"));
    assert!(enter_inserts_newline("TEXT", "two\nlines"));
    assert!(!enter_inserts_newline("TEXT", "one line"));
    assert!(!enter_inserts_newline("INTEGER", "1"));
}

#[test]
fn the_hint_names_the_keys() {
    assert_eq!(
        edit_hint(false),
        "Enter save · Shift+Enter new line · Esc cancel"
    );
    #[cfg(not(target_os = "macos"))]
    assert_eq!(
        edit_hint(true),
        "Ctrl+Enter save · Enter new line · Esc cancel"
    );
}

#[gpui_kit::test]
fn enter_stages_a_single_line_value_and_runs_nothing(cx: &mut TestAppContext) {
    let o = record(cx);
    let run_id = o.model(cx).read_with(cx, |m, _| m.run_id());
    click(cx, &o, ("record-value", 1usize));
    assert!(present(cx, o.win, "record-editor"));
    type_text(cx, &o, "q");
    press(cx, &o, "enter");
    assert!(!present(cx, o.win, "record-editor"));
    assert_eq!(staged(cx, &o, 0, 1), Some(EditValue::Text("q".into())));
    assert_eq!(o.model(cx).read_with(cx, |m, _| m.run_id()), run_id);
}

#[gpui_kit::test]
fn shift_enter_makes_the_value_multi_line_so_enter_then_adds_a_line(cx: &mut TestAppContext) {
    let o = record(cx);
    click(cx, &o, ("record-value", 1usize));
    type_text(cx, &o, "x");
    press(cx, &o, "shift-enter");
    assert_eq!(
        label_of(cx, o.win, "record-hint").as_deref(),
        Some(edit_hint(true))
    );
    press(cx, &o, "enter");
    type_text(cx, &o, "y");
    assert!(present(cx, o.win, "record-editor"));
    press(cx, &o, "ctrl-enter");
    assert_eq!(staged(cx, &o, 0, 1), Some(EditValue::Text("x\n\ny".into())));
}

#[gpui_kit::test]
fn json_and_xml_fields_keep_enter_as_a_newline(cx: &mut TestAppContext) {
    let o = record(cx);
    for col in [2usize, 3] {
        click(cx, &o, ("record-value", col));
        type_text(cx, &o, "{");
        press(cx, &o, "enter");
        assert!(present(cx, o.win, "record-editor"), "column {col}");
        type_text(cx, &o, "}");
        press(cx, &o, "ctrl-enter");
        assert_eq!(staged(cx, &o, 0, col), Some(EditValue::Text("{\n}".into())));
    }
}

#[gpui_kit::test]
fn the_open_field_shows_the_hint_for_its_kind(cx: &mut TestAppContext) {
    let o = record(cx);
    click(cx, &o, ("record-value", 1usize));
    assert_eq!(
        label_of(cx, o.win, "record-hint").as_deref(),
        Some(edit_hint(false))
    );
    press(cx, &o, "escape");
    assert!(!present(cx, o.win, "record-hint"));
    click(cx, &o, ("record-value", 2usize));
    assert_eq!(
        label_of(cx, o.win, "record-hint").as_deref(),
        Some(edit_hint(true))
    );
}

#[gpui_kit::test]
fn enter_stages_in_the_inspector_too(cx: &mut TestAppContext) {
    let o = browse(cx);
    click_id(cx, o.win, "inspector-toggle");
    let pane: Entity<ResultsPane> = o
        .shell
        .read_with(cx, |s, cx| s.tab().unwrap().read(cx).results_pane().clone());
    let table = pane.read_with(cx, |p, _| p.table().clone());
    cx.update_window(o.win, |_, w, cx| {
        table.update(cx, |t, cx| t.set_selected_cell(0, 1, cx));
        table.read(cx).focus_handle(cx).focus(w, cx);
    })
    .unwrap();
    cx.run_until_parked();
    click(cx, &o, ("inspector-value", 1usize));
    assert_eq!(
        label_of(cx, o.win, "inspector-hint").as_deref(),
        Some(edit_hint(false))
    );
    type_text(cx, &o, "w");
    press(cx, &o, "enter");
    assert!(!present(cx, o.win, "inspector-editor"));
    assert_eq!(staged(cx, &o, 0, 1), Some(EditValue::Text("w".into())));
}
