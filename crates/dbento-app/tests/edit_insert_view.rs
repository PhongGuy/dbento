//! Inserted rows in the grid: pinned above the loaded rows, tinted, edited in place.
use crate::support::{Opened, label_of, launched, present, wait_real};
use dbento_app::grid::loaded_selection;
use dbento_app::results_pane::ResultsPane;
use dbento_app::staged::GridRow;
use dbento_core::{EditValue, RelationKind};
use gpui_kit::component::table::{TableDelegate as _, TableSelection};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, Entity, Focusable as _, TestAppContext};

const T: &str = "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT, note TEXT); \
                 INSERT INTO t VALUES (1, 'b', 'x'), (2, 'a', 'y');";

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
/// Selects table cell (row, data column) and focuses the grid.
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
fn add(cx: &mut TestAppContext, o: &Opened) {
    let m = o.model(cx);
    assert_eq!(m.update(cx, |m, cx| m.add_row(cx)), Some(0));
    cx.run_until_parked();
}

#[test]
fn a_selection_on_inserted_rows_is_not_a_loaded_one() {
    assert_eq!(loaded_selection(TableSelection::Cell(0, 2), 1), None);
    assert_eq!(
        loaded_selection(TableSelection::Cell(3, 2), 1),
        Some(TableSelection::Cell(2, 2))
    );
    assert_eq!(
        loaded_selection(TableSelection::Row(2), 2),
        Some(TableSelection::Row(0))
    );
    assert_eq!(
        loaded_selection(TableSelection::Column(1), 4),
        Some(TableSelection::Column(1))
    );
    assert_eq!(
        loaded_selection(TableSelection::Cell(1, 1), 0),
        Some(TableSelection::Cell(1, 1))
    );
}

#[gpui_kit::test]
fn an_added_row_sits_above_row_one(cx: &mut TestAppContext) {
    let o = t(cx);
    add(cx, &o);
    assert!(present(cx, o.win, "row-inserted-0"));
    assert!(!present(cx, o.win, "row-inserted-1"));
    assert_eq!(label_of(cx, o.win, "cell-0-1").as_deref(), Some("DEFAULT"));
    assert_eq!(label_of(cx, o.win, "cell-1-1").as_deref(), Some("b"));
    assert_eq!(label_of(cx, o.win, "cell-2-1").as_deref(), Some("a"));
    let rows = pane(cx, &o).read_with(cx, |p, cx| p.table().read(cx).delegate().rows_count(cx));
    assert_eq!(rows, 3);
}

#[gpui_kit::test]
fn typing_into_an_inserted_cell_stages_it(cx: &mut TestAppContext) {
    let o = t(cx);
    add(cx, &o);
    select(cx, &o, 0, 1);
    press(cx, &o, "enter");
    assert_eq!(pane(cx, &o).read_with(cx, |p, _| p.editing()), Some((0, 1)));
    type_text(cx, &o, "fresh");
    press(cx, &o, "enter");
    let m = o.model(cx);
    assert_eq!(
        m.read_with(cx, |m, _| m.edits().new_cell(0, 1).cloned()),
        Some(EditValue::Text("fresh".into()))
    );
    assert_eq!(label_of(cx, o.win, "cell-0-1").as_deref(), Some("fresh"));
    assert!(
        m.read_with(cx, |m, _| m.edits().row(0).is_none()),
        "loaded row 0 is untouched"
    );
}

#[gpui_kit::test]
fn keys_and_copy_reach_the_loaded_row_under_an_insert(cx: &mut TestAppContext) {
    let o = t(cx);
    add(cx, &o);
    let p = pane(cx, &o);
    select(cx, &o, 0, 1);
    assert_eq!(
        p.update(cx, |p, cx| p.copy_selection(cx)),
        None,
        "an unsaved row has nothing loaded to copy"
    );
    select(cx, &o, 1, 1);
    assert_eq!(
        p.update(cx, |p, cx| p.copy_selection(cx)).as_deref(),
        Some("b")
    );
    press(cx, &o, "delete");
    let m = o.model(cx);
    assert!(m.read_with(cx, |m, _| m.edits().is_deleted(0)));
    assert!(present(cx, o.win, "row-deleted-1"));
    select(cx, &o, 0, 1);
    press(cx, &o, "delete");
    assert_eq!(
        m.read_with(cx, |m, _| m.edits().inserted()),
        0,
        "Delete drops an inserted row"
    );
    assert_eq!(label_of(cx, o.win, "cell-0-1").as_deref(), Some("b"));
    assert!(present(cx, o.win, "row-deleted-0"));
}

#[gpui_kit::test]
fn a_refused_insert_carries_its_mark(cx: &mut TestAppContext) {
    let o = t(cx);
    add(cx, &o);
    let m = o.model(cx);
    assert!(m.update(cx, |m, cx| m.stage_text(GridRow::Inserted(0), 0, "1", cx)));
    assert!(m.update(cx, |m, cx| m.save(cx)));
    wait_real(cx, "the rejection", |cx| {
        m.read_with(cx, |m, _| m.edit_banner().is_some())
    });
    let mark = label_of(cx, o.win, "row-failed-0");
    assert!(
        mark.as_deref().is_some_and(|t| t.contains("UNIQUE")),
        "{mark:?}"
    );
    assert!(present(cx, o.win, "row-inserted-0"));
}

#[gpui_kit::test]
fn a_change_in_inserted_rows_closes_the_editor_and_clears_the_selection(cx: &mut TestAppContext) {
    let o = t(cx);
    select(cx, &o, 0, 1);
    press(cx, &o, "enter");
    type_text(cx, &o, "typed");
    let m = o.model(cx);
    assert_eq!(m.update(cx, |m, cx| m.add_row(cx)), Some(0));
    cx.run_until_parked();
    let p = pane(cx, &o);
    assert_eq!(p.read_with(cx, |p, _| p.editing()), None);
    assert_eq!(
        p.read_with(cx, |p, cx| p.table().read(cx).selection()),
        TableSelection::None
    );
    m.read_with(cx, |m, _| {
        assert!(
            m.edits().row(0).is_none(),
            "nothing staged into the shifted row"
        );
        assert_eq!(m.edits().new_cell(0, 1), None);
    });
    select(cx, &o, 1, 1);
    assert!(m.update(cx, |m, cx| m.undo(cx)));
    cx.run_until_parked();
    assert_eq!(
        p.read_with(cx, |p, cx| p.table().read(cx).selection()),
        TableSelection::None,
        "undo removed the row, so the selection went too"
    );
}

#[gpui_kit::test]
fn closing_the_editor_on_an_insert_returns_focus_to_the_grid(cx: &mut TestAppContext) {
    let o = t(cx);
    select(cx, &o, 0, 1);
    press(cx, &o, "enter");
    let m = o.model(cx);
    assert_eq!(m.update(cx, |m, cx| m.add_row(cx)), Some(0));
    cx.run_until_parked();
    let table = pane(cx, &o).read_with(cx, |p, _| p.table().clone());
    let focused = cx
        .update_window(o.win, |_, w, cx| {
            table.read(cx).focus_handle(cx).is_focused(w)
        })
        .unwrap();
    assert!(focused, "the grid takes focus back from the dropped editor");
}
