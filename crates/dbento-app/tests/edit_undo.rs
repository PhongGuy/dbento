//! Undo and redo of staged edits from the grid's keys and the strip's buttons.
use crate::support::{Opened, label_of, launched, present, wait_real};
use dbento_app::results_pane::ResultsPane;
use dbento_core::{EditValue, RelationKind};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, Entity, Focusable as _, TestAppContext};

const T: &str = "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT); \
                 INSERT INTO t VALUES (1, 'b'), (2, 'a');";

fn t(cx: &mut TestAppContext) -> Opened {
    let o = launched(cx, T, "");
    o.open_relation(cx, "t", RelationKind::Table);
    o.wait_browse(cx, "SELECT *\nFROM main.t\nORDER BY id ASC");
    o
}
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
fn click(cx: &mut TestAppContext, o: &Opened, id: &'static str) {
    cx.update_window(o.win, |_, w, cx| w.click(id, cx)).unwrap();
    cx.run_until_parked();
}
fn stage_z(cx: &mut TestAppContext, o: &Opened) {
    let m = o.model(cx);
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 1, "z", cx)));
    cx.run_until_parked();
}
fn staged(cx: &mut TestAppContext, o: &Opened) -> Option<EditValue> {
    o.model(cx)
        .read_with(cx, |m, _| m.edits().cell(0, 1).cloned())
}

#[gpui_kit::test]
fn ctrl_z_and_ctrl_shift_z_in_the_grid_step_the_edits(cx: &mut TestAppContext) {
    let o = t(cx);
    stage_z(cx, &o);
    select(cx, &o, 1, 1);
    press(cx, &o, "ctrl-z");
    assert_eq!(staged(cx, &o), None);
    assert!(!present(cx, o.win, "pending-changes"));
    o.assert_inert(cx, "save-edits");
    assert!(o.strip_enabled(cx, "redo-edits"), "redo stays reachable");
    press(cx, &o, "ctrl-shift-z");
    assert_eq!(staged(cx, &o), Some(EditValue::Text("z".into())));
    assert_eq!(label_of(cx, o.win, "cell-0-1").as_deref(), Some("z"));
}

#[gpui_kit::test]
fn the_buttons_follow_the_history(cx: &mut TestAppContext) {
    let o = t(cx);
    o.assert_inert(cx, "undo-edits");
    o.assert_inert(cx, "redo-edits");
    stage_z(cx, &o);
    assert!(o.strip_enabled(cx, "undo-edits"));
    o.assert_inert(cx, "redo-edits");
    click(cx, &o, "undo-edits");
    assert_eq!(staged(cx, &o), None);
    o.assert_inert(cx, "revert-edits");
    click(cx, &o, "redo-edits");
    assert_eq!(
        label_of(cx, o.win, "pending-changes").as_deref(),
        Some("1 pending change")
    );
}

#[gpui_kit::test]
fn ctrl_z_in_the_cell_editor_undoes_typing_not_the_edits(cx: &mut TestAppContext) {
    let o = t(cx);
    stage_z(cx, &o);
    select(cx, &o, 1, 1);
    press(cx, &o, "enter");
    assert_eq!(pane(cx, &o).read_with(cx, |p, _| p.editing()), Some((1, 1)));
    cx.update_window(o.win, |_, w, cx| w.input("abc", cx))
        .unwrap();
    cx.run_until_parked();
    press(cx, &o, "ctrl-z");
    assert_eq!(staged(cx, &o), Some(EditValue::Text("z".into())));
    assert_eq!(pane(cx, &o).read_with(cx, |p, _| p.editing()), Some((1, 1)));
}

#[gpui_kit::test]
fn ctrl_shift_z_in_the_cell_editor_redoes_nothing(cx: &mut TestAppContext) {
    let o = t(cx);
    stage_z(cx, &o);
    let m = o.model(cx);
    assert!(m.update(cx, |m, cx| m.undo(cx)));
    select(cx, &o, 1, 1);
    press(cx, &o, "enter");
    press(cx, &o, "ctrl-shift-z");
    assert_eq!(staged(cx, &o), None);
    assert!(m.read_with(cx, |m, _| m.can_redo()));
}

#[gpui_kit::test]
fn undoing_to_nothing_staged_clears_the_rejection_banner(cx: &mut TestAppContext) {
    let o = t(cx);
    stage_z(cx, &o);
    rusqlite::Connection::open(o.fx.dir.path().join("data/app.db"))
        .unwrap()
        .execute_batch("DELETE FROM t WHERE id = 1")
        .unwrap();
    click(cx, &o, "save-edits");
    wait_real(cx, "the banner", |cx| present(cx, o.win, "edit-banner"));
    let m = o.model(cx);
    wait_real(cx, "the save to end", |cx| {
        m.read_with(cx, |m, _| m.can_undo())
    });
    stage_two(cx, &o);
    click(cx, &o, "undo-edits");
    assert!(
        present(cx, o.win, "edit-banner"),
        "still staged, so the banner still applies"
    );
    click(cx, &o, "undo-edits");
    assert!(!present(cx, o.win, "edit-banner"));
    click(cx, &o, "redo-edits");
    assert!(!present(cx, o.win, "edit-banner"), "the banner stays gone");
}

/// Stages a second cell, so one undo leaves the first staged.
fn stage_two(cx: &mut TestAppContext, o: &Opened) {
    let m = o.model(cx);
    assert!(m.update(cx, |m, cx| m.stage_cell(1, 1, "y", cx)));
    cx.run_until_parked();
}

#[gpui_kit::test]
fn the_undo_button_waits_for_the_cell_editor_to_close(cx: &mut TestAppContext) {
    let o = t(cx);
    stage_z(cx, &o);
    select(cx, &o, 1, 1);
    press(cx, &o, "enter");
    click(cx, &o, "undo-edits");
    assert_eq!(staged(cx, &o), Some(EditValue::Text("z".into())));
    press(cx, &o, "escape");
    click(cx, &o, "undo-edits");
    assert_eq!(staged(cx, &o), None);
}
