//! The Grid / Record toggle: one row as a Name / Value form, with row navigation.
use crate::support::{Opened, label_of, launched, opened, present};
use dbento_app::results_pane::{ResultView, ResultsPane};
use dbento_core::RelationKind;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, ElementId, Entity, Focusable as _, TestAppContext};

const T: &str = "CREATE TABLE a(id INTEGER PRIMARY KEY, name TEXT); \
                 CREATE TABLE b(id INTEGER PRIMARY KEY, a_id INTEGER REFERENCES a(id), note TEXT); \
                 INSERT INTO a VALUES (1, 'x'); \
                 INSERT INTO b VALUES (1, 1, 'first'), (2, NULL, 'second');";

fn b(cx: &mut TestAppContext) -> Opened {
    let o = launched(cx, T, "");
    o.open_relation(cx, "b", RelationKind::Table);
    o.wait_browse(cx, "SELECT *\nFROM main.b\nORDER BY id ASC");
    o
}
fn pane(cx: &mut TestAppContext, o: &Opened) -> Entity<ResultsPane> {
    o.shell
        .read_with(cx, |s, cx| s.tab().unwrap().read(cx).results_pane().clone())
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
fn the_toggle_swaps_the_grid_for_the_form(cx: &mut TestAppContext) {
    let o = b(cx);
    assert!(present(cx, o.win, "row-view-expand"));
    click(cx, &o, "row-view-expand");
    assert!(present(cx, o.win, "record-view"));
    assert!(!present(cx, o.win, "result-grid"));
    assert_eq!(
        label(cx, &o, "record-position").as_deref(),
        Some("Row 1 of 2")
    );
    assert_eq!(
        label(cx, &o, ("record-name", 1usize)).as_deref(),
        Some("a_id")
    );
    assert_eq!(
        label(cx, &o, ("record-type", 1usize)).as_deref(),
        Some("INTEGER → a")
    );
    assert!(present(cx, o.win, ("record-pk", 0usize)));
    assert!(present(cx, o.win, ("record-fk", 1usize)));
    assert!(!present(cx, o.win, ("record-fk", 2usize)));
    assert_eq!(
        label(cx, &o, ("record-value", 2usize)).as_deref(),
        Some("first")
    );
    click(cx, &o, "row-view-expand");
    assert!(present(cx, o.win, "result-grid"));
    assert!(!present(cx, o.win, "record-view"));
}

#[gpui_kit::test]
fn the_record_view_starts_on_the_selection_and_steps_within_the_result(cx: &mut TestAppContext) {
    let o = b(cx);
    let p = pane(cx, &o);
    let table = p.read_with(cx, |p, _| p.table().clone());
    cx.update_window(o.win, |_, w, cx| {
        table.update(cx, |t, cx| t.set_selected_cell(1, 2, cx));
        table.read(cx).focus_handle(cx).focus(w, cx);
    })
    .unwrap();
    click(cx, &o, "row-view-expand");
    assert_eq!(p.read_with(cx, |p, _| p.view()), ResultView::Record);
    assert_eq!(
        label(cx, &o, "record-position").as_deref(),
        Some("Row 2 of 2")
    );
    assert_eq!(
        label(cx, &o, ("record-value", 2usize)).as_deref(),
        Some("second")
    );
    assert_eq!(
        label(cx, &o, ("record-value", 1usize)).as_deref(),
        Some("NULL")
    );
    click(cx, &o, "record-next");
    assert_eq!(
        label(cx, &o, "record-position").as_deref(),
        Some("Row 2 of 2")
    );
    click(cx, &o, "record-prev");
    assert_eq!(
        label(cx, &o, "record-position").as_deref(),
        Some("Row 1 of 2")
    );
    click(cx, &o, "row-view-expand");
    assert_eq!(p.read_with(cx, |p, cx| p.current_row(cx)), Some(0));
}

#[gpui_kit::test]
fn the_record_view_shows_a_free_result_and_clamps_on_a_rerun(cx: &mut TestAppContext) {
    let o = opened(cx, T, "", 0);
    o.run(cx, "SELECT 1 AS n UNION ALL SELECT 2");
    o.idle(cx);
    click(cx, &o, "row-view-expand");
    assert!(!present(cx, o.win, ("record-pk", 0usize)));
    click(cx, &o, "record-next");
    assert_eq!(
        label(cx, &o, "record-position").as_deref(),
        Some("Row 2 of 2")
    );
    assert_eq!(
        label(cx, &o, ("record-value", 0usize)).as_deref(),
        Some("2")
    );
    o.run(cx, "SELECT 5 AS n");
    o.idle(cx);
    assert_eq!(
        label(cx, &o, "record-position").as_deref(),
        Some("Row 1 of 1")
    );
    assert_eq!(
        label(cx, &o, ("record-value", 0usize)).as_deref(),
        Some("5")
    );
    o.run(cx, "SELECT 1 AS n WHERE 0");
    o.idle(cx);
    assert!(present(cx, o.win, "record-empty"));
}

#[gpui_kit::test]
fn the_record_row_keeps_its_grid_row_when_inserted_rows_change(cx: &mut TestAppContext) {
    let o = b(cx);
    click(cx, &o, "row-view-expand");
    click(cx, &o, "record-next");
    let m = o.model(cx);
    assert_eq!(m.update(cx, |m, cx| m.add_row(cx)), Some(0));
    cx.run_until_parked();
    assert_eq!(
        label(cx, &o, "record-position").as_deref(),
        Some("Row 2 of 2")
    );
    assert_eq!(
        label(cx, &o, ("record-value", 2usize)).as_deref(),
        Some("second")
    );
    // Ctrl+Z reaches the history from the record view.
    cx.update_window(o.win, |_, w, cx| w.press("ctrl-z", cx))
        .unwrap();
    cx.run_until_parked();
    assert_eq!(m.read_with(cx, |m, _| m.edits().inserted()), 0);
    assert_eq!(
        label(cx, &o, "record-position").as_deref(),
        Some("Row 2 of 2")
    );
    assert_eq!(m.update(cx, |m, cx| m.add_row(cx)), Some(0));
    cx.run_until_parked();
    click(cx, &o, "record-prev");
    click(cx, &o, "record-prev");
    assert_eq!(
        label(cx, &o, "record-position").as_deref(),
        Some("New row 1")
    );
    assert!(m.update(cx, |m, cx| m.undo(cx)));
    cx.run_until_parked();
    assert_eq!(
        label(cx, &o, "record-position").as_deref(),
        Some("Row 1 of 2")
    );
}

#[gpui_kit::test]
fn the_record_form_offers_no_grid_menu(cx: &mut TestAppContext) {
    let o = b(cx);
    click(cx, &o, "row-view-expand");
    cx.update_window(o.win, |_, w, cx| {
        w.right_click(("record-value", 2usize), cx)
    })
    .unwrap();
    cx.run_until_parked();
    assert!(!present(cx, o.win, "grid-menu-add-row"));
    assert!(!present(cx, o.win, "grid-menu-edit"));
}

#[gpui_kit::test]
fn undoing_the_shown_inserted_row_resets_the_record_to_the_first_row(cx: &mut TestAppContext) {
    let o = b(cx);
    let m = o.model(cx);
    assert_eq!(m.update(cx, |m, cx| m.add_row(cx)), Some(0));
    assert_eq!(m.update(cx, |m, cx| m.add_row(cx)), Some(1));
    cx.run_until_parked();
    click(cx, &o, "row-view-expand");
    click(cx, &o, "record-next");
    assert_eq!(
        label(cx, &o, "record-position").as_deref(),
        Some("New row 2")
    );
    assert!(m.update(cx, |m, cx| m.undo(cx)));
    cx.run_until_parked();
    assert_eq!(
        label(cx, &o, "record-position").as_deref(),
        Some("New row 1")
    );
}

#[gpui_kit::test]
fn setting_the_active_view_again_keeps_the_row(cx: &mut TestAppContext) {
    let o = b(cx);
    click(cx, &o, "row-view-expand");
    click(cx, &o, "record-next");
    // A new inserted row clears the grid selection the record view would restart from.
    let m = o.model(cx);
    assert_eq!(m.update(cx, |m, cx| m.add_row(cx)), Some(0));
    cx.run_until_parked();
    assert_eq!(
        label(cx, &o, "record-position").as_deref(),
        Some("Row 2 of 2")
    );
    let p = pane(cx, &o);
    cx.update_window(o.win, |_, w, cx| {
        p.update(cx, |p, cx| p.set_view(ResultView::Record, w, cx))
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(
        label(cx, &o, "record-position").as_deref(),
        Some("Row 2 of 2")
    );
}

#[gpui_kit::test]
fn redo_keys_reach_the_history_from_the_record_view(cx: &mut TestAppContext) {
    let o = b(cx);
    click(cx, &o, "row-view-expand");
    let m = o.model(cx);
    assert_eq!(m.update(cx, |m, cx| m.add_row(cx)), Some(0));
    cx.run_until_parked();
    click(cx, &o, "record-next");
    cx.update_window(o.win, |_, w, cx| w.press("ctrl-z", cx))
        .unwrap();
    cx.run_until_parked();
    assert_eq!(m.read_with(cx, |m, _| m.edits().inserted()), 0);
    cx.update_window(o.win, |_, w, cx| w.press("ctrl-shift-z", cx))
        .unwrap();
    cx.run_until_parked();
    assert_eq!(m.read_with(cx, |m, _| m.edits().inserted()), 1);
}

#[gpui_kit::test]
fn grid_keys_do_nothing_in_the_record_view(cx: &mut TestAppContext) {
    let o = b(cx);
    click(cx, &o, "row-view-expand");
    cx.update_window(o.win, |_, w, cx| w.press("delete", cx))
        .unwrap();
    cx.run_until_parked();
    let m = o.model(cx);
    assert!(m.read_with(cx, |m, _| m.edits().is_empty()));
    assert!(present(cx, o.win, "record-view"));
}

#[gpui_kit::test]
fn the_form_has_a_name_and_value_header(cx: &mut TestAppContext) {
    let o = b(cx);
    click(cx, &o, "row-view-expand");
    assert!(present(cx, o.win, "record-header"));
}

#[gpui_kit::test]
fn field_names_share_one_left_edge_with_or_without_a_marker(cx: &mut TestAppContext) {
    let o = b(cx);
    click(cx, &o, "row-view-expand");
    // Column 0 is the primary key, 1 a foreign key, 2 has no marker.
    cx.update_window(o.win, |_, w, cx| {
        w.render_frame(cx);
        let lefts: Vec<_> = (0..3usize)
            .map(|c| f32::from(w.find(("record-name", c)).bounds().left()))
            .collect();
        assert!(
            lefts.iter().all(|l| (l - lefts[0]).abs() < 0.5),
            "{lefts:?}"
        );
    })
    .unwrap();
}

#[gpui_kit::test]
fn the_name_heading_lines_up_with_the_field_names(cx: &mut TestAppContext) {
    let o = b(cx);
    click(cx, &o, "row-view-expand");
    cx.update_window(o.win, |_, w, cx| {
        w.render_frame(cx);
        let heading = f32::from(w.find("record-heading-name").bounds().left());
        let name = f32::from(w.find(("record-name", 2usize)).bounds().left());
        assert!((heading - name).abs() < 0.5, "{heading} vs {name}");
    })
    .unwrap();
}

#[gpui_kit::test]
fn a_row_staged_for_deletion_reads_as_deleted(cx: &mut TestAppContext) {
    use dbento_app::staged::GridRow::Loaded;
    let o = b(cx);
    click(cx, &o, "row-view-expand");
    assert!(!present(cx, o.win, "record-deleted"));
    let m = o.model(cx);
    assert!(m.update(cx, |m, cx| m.delete_at(Loaded(0), cx)));
    cx.run_until_parked();
    assert_eq!(label(cx, &o, "record-deleted").as_deref(), Some("Deleted"));
    click(cx, &o, "record-next");
    assert!(!present(cx, o.win, "record-deleted"));
}
