//! Editable browse grids always show the edit strip; a blocked one says why.
mod support;
use dbento_app::results_pane::ResultsPane;
use dbento_core::RelationKind;
use gpui_kit::{AppContext as _, Entity, Focusable as _, TestAppContext};
use support::{Opened, click_id, label_of, launched, present};

const T: &str = "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT); \
                 INSERT INTO t VALUES (1, 'b'), (2, 'a');";

fn t(cx: &mut TestAppContext, extra: &str) -> Opened {
    let o = launched(cx, T, extra);
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

#[gpui_kit::test]
fn a_fresh_browse_grid_shows_the_strip_with_what_applies(cx: &mut TestAppContext) {
    let o = t(cx, "");
    assert!(present(cx, o.win, "edit-strip"));
    assert!(o.strip_enabled(cx, "edit-strip-add"));
    for id in [
        "edit-strip-duplicate",
        "edit-strip-delete",
        "undo-edits",
        "redo-edits",
        "revert-edits",
        "save-edits",
    ] {
        o.assert_inert(cx, id);
    }
    assert!(!present(cx, o.win, "pending-changes"));
    select(cx, &o, 1, 1);
    assert!(o.strip_enabled(cx, "edit-strip-duplicate"));
    click_id(cx, o.win, "edit-strip-delete");
    assert!(o.model(cx).read_with(cx, |m, _| m.edits().is_deleted(1)));
    assert_eq!(
        label_of(cx, o.win, "pending-changes").as_deref(),
        Some("1 pending change")
    );
    assert!(o.strip_enabled(cx, "save-edits"));
    o.assert_inert(cx, "edit-strip-delete");
    click_id(cx, o.win, "edit-strip-add");
    assert_eq!(o.model(cx).read_with(cx, |m, _| m.edits().inserted()), 1);
}

#[gpui_kit::test]
fn a_read_only_connection_says_why_instead(cx: &mut TestAppContext) {
    let o = t(cx, "read_only = true");
    assert!(present(cx, o.win, "edit-strip"));
    assert_eq!(
        label_of(cx, o.win, "browse-not-editable").as_deref(),
        Some("read-only connection")
    );
    assert!(!present(cx, o.win, "edit-strip-add"));
}
