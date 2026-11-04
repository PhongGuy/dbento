//! The inspector's form follows the current row and tab; its buttons edit, duplicate, delete.
mod support;
use dbento_app::keymap::UndoEdit;
use dbento_app::results_pane::ResultsPane;
use dbento_app::staged::GridRow;
use dbento_core::{EditValue, RelationKind};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, ElementId, Entity, Focusable as _, TestAppContext};
use support::{Opened, click_id, label_of, launched, present, wait_real};

const T: &str = "CREATE TABLE a(id INTEGER PRIMARY KEY, name TEXT); \
                 CREATE TABLE b(id INTEGER PRIMARY KEY, a_id INTEGER REFERENCES a(id), note TEXT); \
                 INSERT INTO a VALUES (1, 'x'); \
                 INSERT INTO b VALUES (1, 1, 'n'), (2, NULL, 'm');";

fn b(cx: &mut TestAppContext) -> Opened {
    let o = launched(cx, T, "");
    o.open_relation(cx, "b", RelationKind::Table);
    o.wait_browse(cx, "SELECT *\nFROM main.b\nORDER BY id ASC");
    click_id(cx, o.win, "inspector-toggle");
    o
}
fn pane(cx: &mut TestAppContext, o: &Opened) -> Entity<ResultsPane> {
    o.shell
        .read_with(cx, |s, cx| s.tab().unwrap().read(cx).results_pane().clone())
}
fn select(cx: &mut TestAppContext, o: &Opened, row: usize) {
    let table = pane(cx, o).read_with(cx, |p, _| p.table().clone());
    cx.update_window(o.win, |_, w, cx| {
        table.update(cx, |t, cx| t.set_selected_cell(row, 1, cx));
        table.read(cx).focus_handle(cx).focus(w, cx);
    })
    .unwrap();
    cx.run_until_parked();
}
fn click(cx: &mut TestAppContext, o: &Opened, id: impl Into<ElementId>) {
    let id = id.into();
    cx.update_window(o.win, |_, w, cx| w.click(id, cx)).unwrap();
    cx.run_until_parked();
}
fn label(cx: &mut TestAppContext, o: &Opened, id: impl Into<ElementId>) -> Option<String> {
    label_of(cx, o.win, id)
}

#[gpui_kit::test]
fn the_expanded_record_and_the_inspector_show_the_same_row(cx: &mut TestAppContext) {
    let o = b(cx);
    select(cx, &o, 1);
    assert_eq!(
        label(cx, &o, ("inspector-value", 2usize)).as_deref(),
        Some("m")
    );
    assert!(present(cx, o.win, ("inspector-pk", 0usize)));
    click(cx, &o, "row-view-expand");
    assert_eq!(
        label(cx, &o, ("record-value", 2usize)).as_deref(),
        Some("m")
    );
    click(cx, &o, "record-prev");
    assert_eq!(
        label(cx, &o, "record-position").as_deref(),
        Some("Row 1 of 2")
    );
    assert_eq!(
        label(cx, &o, ("record-value", 2usize)).as_deref(),
        Some("n")
    );
    click(cx, &o, "row-view-expand");
    assert_eq!(label(cx, &o, "inspector-title").as_deref(), Some("Row 1"));
    assert_eq!(
        label(cx, &o, ("inspector-value", 2usize)).as_deref(),
        Some("n")
    );
}

#[gpui_kit::test]
fn edit_opens_the_first_non_key_field_and_stages(cx: &mut TestAppContext) {
    let o = b(cx);
    select(cx, &o, 0);
    click(cx, &o, "inspector-edit");
    assert!(present(cx, o.win, "inspector-editor"));
    cx.update_window(o.win, |_, w, cx| {
        w.input("2", cx);
        w.press("ctrl-enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    let m = o.model(cx);
    m.read_with(cx, |m, _| {
        assert_eq!(m.edits().cell(0, 1), Some(&EditValue::Text("2".into())));
    });
}

#[gpui_kit::test]
fn duplicate_selects_the_new_row_and_delete_marks_the_row(cx: &mut TestAppContext) {
    let o = b(cx);
    select(cx, &o, 1);
    click(cx, &o, "inspector-duplicate");
    let m = o.model(cx);
    assert_eq!(m.read_with(cx, |m, _| m.edits().inserted()), 1);
    assert_eq!(
        label(cx, &o, "inspector-title").as_deref(),
        Some("New row 1")
    );
    assert!(!present(cx, o.win, "inspector-identity"));
    select(cx, &o, 2);
    assert_eq!(m.read_with(cx, |m, _| m.grid_row(2)), GridRow::Loaded(1));
    click(cx, &o, "inspector-delete");
    assert!(m.read_with(cx, |m, _| m.edits().is_deleted(1)));
}

#[gpui_kit::test]
fn removing_an_inserted_row_keeps_the_record_views_row_in_the_inspector(cx: &mut TestAppContext) {
    let o = b(cx);
    click(cx, &o, "row-view-expand");
    click(cx, &o, "record-next");
    let position = |cx: &mut TestAppContext| label(cx, &o, "record-position");
    assert_eq!(position(cx).as_deref(), Some("Row 2 of 2"));
    let m = o.model(cx);
    assert_eq!(m.update(cx, |m, cx| m.add_row(cx)), Some(0));
    cx.run_until_parked();
    assert_eq!(position(cx).as_deref(), Some("Row 2 of 2"));
    assert!(m.update(cx, |m, cx| m.undo(cx)));
    cx.run_until_parked();
    assert_eq!(position(cx).as_deref(), Some("Row 2 of 2"));
    assert_eq!(
        label(cx, &o, ("record-value", 2usize)).as_deref(),
        Some("m")
    );
    click(cx, &o, "row-view-expand");
    select(cx, &o, 0);
    click(cx, &o, "inspector-duplicate");
    assert_eq!(
        label(cx, &o, "inspector-title").as_deref(),
        Some("New row 1")
    );
    click(cx, &o, "inspector-delete");
    assert_eq!(m.read_with(cx, |m, _| m.edits().inserted()), 0);
    assert!(present(cx, o.win, "inspector-empty"));
}

#[gpui_kit::test]
fn the_inspector_follows_the_active_tab(cx: &mut TestAppContext) {
    let o = b(cx);
    select(cx, &o, 0);
    assert_eq!(label(cx, &o, "inspector-table").as_deref(), Some("main.b"));
    o.open_relation(cx, "a", RelationKind::Table);
    o.wait_browse(cx, "SELECT *\nFROM main.a\nORDER BY id ASC");
    assert!(present(cx, o.win, "inspector-empty"));
    select(cx, &o, 0);
    assert_eq!(label(cx, &o, "inspector-table").as_deref(), Some("main.a"));
    assert_eq!(
        label(cx, &o, ("inspector-value", 1usize)).as_deref(),
        Some("x")
    );
    o.run(cx, "SELECT 1 AS n");
    o.idle(cx);
    select(cx, &o, 0);
    assert!(!present(cx, o.win, "inspector-edit"));
}

#[gpui_kit::test]
fn a_row_shift_drops_the_inspectors_open_field_unstaged(cx: &mut TestAppContext) {
    let o = b(cx);
    select(cx, &o, 0);
    click(cx, &o, "inspector-edit");
    cx.update_window(o.win, |_, w, cx| w.input("7", cx))
        .unwrap();
    let m = o.model(cx);
    assert_eq!(m.update(cx, |m, cx| m.add_row(cx)), Some(0));
    cx.run_until_parked();
    assert!(!present(cx, o.win, "inspector-editor"));
    m.read_with(cx, |m, _| {
        assert_eq!(m.edits().cell(0, 1), None);
        assert_eq!(m.edits().new_cell(0, 1), None);
    });
}

#[gpui_kit::test]
fn undo_waits_while_the_inspectors_field_is_open(cx: &mut TestAppContext) {
    let o = b(cx);
    select(cx, &o, 1);
    let m = o.model(cx);
    m.update(cx, |m, cx| {
        m.set_value(GridRow::Loaded(0), 2, EditValue::Text("z".into()), cx)
    });
    cx.run_until_parked();
    click(cx, &o, "inspector-edit");
    cx.update_window(o.win, |_, w, cx| w.dispatch_action(Box::new(UndoEdit), cx))
        .unwrap();
    cx.run_until_parked();
    assert_eq!(
        m.read_with(cx, |m, _| m.edits().cell(0, 2).cloned()),
        Some(EditValue::Text("z".into()))
    );
}

#[gpui_kit::test]
fn leaving_the_inspectors_field_stages_it(cx: &mut TestAppContext) {
    let o = b(cx);
    // Focus events reach listeners only in an active window, as the app's is when clicked.
    cx.update_window(o.win, |_, w, _| w.activate_window())
        .unwrap();
    select(cx, &o, 0);
    click(cx, &o, "inspector-edit");
    cx.update_window(o.win, |_, w, cx| w.input("9", cx))
        .unwrap();
    select(cx, &o, 0);
    assert!(!present(cx, o.win, "inspector-editor"));
    let m = o.model(cx);
    m.read_with(cx, |m, _| {
        assert_eq!(m.edits().cell(0, 1), Some(&EditValue::Text("9".into())));
    });
}

// Switching tabs by key gives the inspector's field no blur; it stages before the form swaps.
#[gpui_kit::test]
fn switching_tabs_by_key_stages_the_inspectors_open_field(cx: &mut TestAppContext) {
    let o = b(cx);
    cx.update_window(o.win, |_, w, _| w.activate_window())
        .unwrap();
    let m = o.model(cx);
    o.open_relation(cx, "a", RelationKind::Table);
    o.wait_browse(cx, "SELECT *\nFROM main.a\nORDER BY id ASC");
    click(cx, &o, ("tab", 0usize));
    select(cx, &o, 0);
    click(cx, &o, "inspector-edit");
    cx.update_window(o.win, |_, w, cx| w.input("9", cx))
        .unwrap();
    cx.update_window(o.win, |_, w, cx| w.press("secondary-t", cx))
        .unwrap();
    cx.run_until_parked();
    m.read_with(cx, |m, _| {
        assert_eq!(m.edits().cell(0, 1), Some(&EditValue::Text("9".into())));
    });
}

#[gpui_kit::test]
fn closing_the_last_tab_empties_the_inspector(cx: &mut TestAppContext) {
    let o = b(cx);
    select(cx, &o, 0);
    assert!(present(cx, o.win, "inspector-title"));
    let shell = o.shell.clone();
    while shell.read_with(cx, |s, _| !s.tabs().is_empty()) {
        cx.update_window(o.win, |_, w, cx| {
            shell.update(cx, |s, cx| s.close_tab_at(0, w, cx))
        })
        .unwrap();
        cx.run_until_parked();
    }
    assert!(present(cx, o.win, "inspector-empty"));
    assert!(!present(cx, o.win, "inspector-title"));
}

#[gpui_kit::test]
fn switching_apps_keeps_the_inspectors_field_open(cx: &mut TestAppContext) {
    let o = b(cx);
    cx.update_window(o.win, |_, w, _| w.activate_window())
        .unwrap();
    select(cx, &o, 0);
    click(cx, &o, "inspector-edit");
    gpui_kit::VisualTestContext::from_window(o.win, cx).deactivate_window();
    assert!(present(cx, o.win, "inspector-editor"));
}

#[gpui_kit::test]
fn a_button_press_takes_the_keys_to_the_inspector(cx: &mut TestAppContext) {
    let o = b(cx);
    select(cx, &o, 1);
    click(cx, &o, "inspector-delete");
    let inspector = o.shell.read_with(cx, |s, _| s.inspector().clone());
    let held = cx
        .update_window(o.win, |_, w, cx| {
            inspector.read(cx).focus_handle(cx).contains_focused(w, cx)
        })
        .unwrap();
    // The kit's buttons refuse focus on press, which also stops the panel's `track_focus`.
    assert!(held, "the grid kept the keys");
}

#[gpui_kit::test]
fn undo_from_the_inspector_steps_the_history(cx: &mut TestAppContext) {
    let o = b(cx);
    select(cx, &o, 1);
    let m = o.model(cx);
    m.update(cx, |m, cx| {
        m.set_value(GridRow::Loaded(0), 2, EditValue::Text("z".into()), cx)
    });
    cx.run_until_parked();
    let form = o
        .shell
        .read_with(cx, |s, cx| s.inspector().read(cx).form().unwrap().clone());
    cx.update_window(o.win, |_, w, cx| {
        form.read(cx).focus_handle(cx).focus(w, cx);
        w.dispatch_action(Box::new(UndoEdit), cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(m.read_with(cx, |m, _| m.edits().cell(0, 2).cloned()), None);
}

#[gpui_kit::test]
fn opening_a_field_in_one_form_stages_the_others_first(cx: &mut TestAppContext) {
    let o = b(cx);
    cx.update_window(o.win, |_, w, _| w.activate_window())
        .unwrap();
    select(cx, &o, 0);
    click(cx, &o, ("inspector-value", 2usize));
    cx.update_window(o.win, |_, w, cx| w.input("stale", cx))
        .unwrap();
    // The expanded record hides the dock, so its form opens second here; leaving the
    // inspector's field stages it.
    click(cx, &o, "row-view-expand");
    let m = o.model(cx);
    assert_eq!(
        m.read_with(cx, |m, _| m.edits().cell(0, 2).cloned()),
        Some(EditValue::Text("stale".into()))
    );
    click(cx, &o, ("record-value", 2usize));
    cx.update_window(o.win, |_, w, cx| {
        w.input("fresh", cx);
        w.press("ctrl-enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    click(cx, &o, "row-view-expand");
    assert_eq!(
        m.read_with(cx, |m, _| m.edits().cell(0, 2).cloned()),
        Some(EditValue::Text("fresh".into()))
    );
}

#[gpui_kit::test]
fn the_buttons_wait_for_a_save(cx: &mut TestAppContext) {
    let o = b(cx);
    select(cx, &o, 0);
    let inspector = o.shell.read_with(cx, |s, _| s.inspector().clone());
    assert!(inspector.read_with(cx, |i, cx| i.actions_live(cx)));
    let m = o.model(cx);
    m.update(cx, |m, cx| {
        m.set_value(GridRow::Loaded(1), 2, EditValue::Text("z".into()), cx)
    });
    click(cx, &o, "save-edits");
    wait_real(cx, "the prompt", |cx| present(cx, o.win, "confirm-dialog"));
    assert!(!inspector.read_with(cx, |i, cx| i.actions_live(cx)));
}

#[gpui_kit::test]
fn ctrl_s_in_the_inspectors_field_stages_and_saves(cx: &mut TestAppContext) {
    let o = b(cx);
    select(cx, &o, 1);
    click(cx, &o, "inspector-edit");
    cx.update_window(o.win, |_, w, cx| {
        w.input("1", cx);
        w.press("ctrl-s", cx);
    })
    .unwrap();
    wait_real(cx, "the prompt", |cx| present(cx, o.win, "confirm-dialog"));
    let m = o.model(cx);
    assert_eq!(
        m.read_with(cx, |m, _| m.edits().cell(1, 1).cloned()),
        Some(EditValue::Text("1".into()))
    );
}

// The Save button takes no focus, so the inspector's open field must be staged by the save
// itself rather than by the form's focus-out. Row 1's NULL key takes a real `a` id.
#[gpui_kit::test]
fn the_save_button_stages_an_open_inspector_field(cx: &mut TestAppContext) {
    let o = b(cx);
    let m = o.model(cx);
    m.update(cx, |m, cx| {
        m.set_value(GridRow::Loaded(0), 2, EditValue::Text("z".into()), cx)
    });
    select(cx, &o, 1);
    click(cx, &o, "inspector-edit");
    cx.update_window(o.win, |_, w, cx| w.input("1", cx))
        .unwrap();
    cx.run_until_parked();
    click(cx, &o, "save-edits");
    support::wait_prompt(cx, &m);
    m.read_with(cx, |m, _| {
        assert_eq!(m.edits().cell(1, 1), Some(&EditValue::Text("1".into())));
        assert_eq!(m.edits().len(), 2);
    });
}

// Like Ctrl+Z, the Undo button leaves history alone while an inspector field is open.
#[gpui_kit::test]
fn the_undo_button_waits_for_an_open_inspector_field(cx: &mut TestAppContext) {
    let o = b(cx);
    let m = o.model(cx);
    m.update(cx, |m, cx| {
        m.set_value(GridRow::Loaded(1), 2, EditValue::Text("z".into()), cx)
    });
    select(cx, &o, 0);
    click(cx, &o, "inspector-edit");
    cx.update_window(o.win, |_, w, cx| w.input("9", cx))
        .unwrap();
    cx.run_until_parked();
    click(cx, &o, "undo-edits");
    assert!(present(cx, o.win, "inspector-editor"));
    m.read_with(cx, |m, _| {
        assert_eq!(m.edits().cell(1, 2), Some(&EditValue::Text("z".into())));
    });
}
