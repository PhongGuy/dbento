//! Hidden and fitted columns: the view's index maps, and a grid that hides, shows and fits.
mod support;
use dbento_app::grid::{ColumnView, fit_width};
use dbento_app::results::ResultStore;
use dbento_app::results_pane::ResultsPane;
use dbento_core::{EditValue, RelationKind};
use gpui_kit::component::table::{TableDelegate as _, TableEvent, TableSelection};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, Entity, TestAppContext, px};
use support::{Opened, launched, opened, present};

const T: &str = "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT NOT NULL, note TEXT); \
                 INSERT INTO t VALUES (1, 'b', 'x'), (2, 'a', NULL);";

fn pane(cx: &mut TestAppContext, o: &Opened) -> Entity<ResultsPane> {
    o.shell
        .read_with(cx, |s, cx| s.tab().unwrap().read(cx).results_pane().clone())
}
fn headers(cx: &mut TestAppContext, o: &Opened) -> Vec<String> {
    let table = pane(cx, o).read_with(cx, |p, _| p.table().clone());
    cx.run_until_parked();
    table.read_with(cx, |t, cx| t.headers(cx))
}

#[test]
fn hidden_columns_shift_the_visible_indexes() {
    let mut v = ColumnView::new(vec!["id".into(), "name".into(), "note".into()]);
    assert_eq!(v.data_col(0), None);
    assert_eq!(v.data_col(2), Some(1));
    assert!(v.hide(0));
    assert!(!v.hide(0), "already hidden");
    assert_eq!(v.visible_count(), 2);
    assert_eq!(
        (v.data_col(1), v.data_col(2), v.data_col(3)),
        (Some(1), Some(2), None)
    );
    assert_eq!((v.table_col(0), v.table_col(2)), (None, Some(2)));
    assert_eq!(
        v.full_selection(TableSelection::Cell(4, 1)),
        TableSelection::Cell(4, 2)
    );
    assert_eq!(
        v.full_selection(TableSelection::Column(2)),
        TableSelection::Column(3)
    );
    assert_eq!(
        v.full_selection(TableSelection::Cell(4, 0)),
        TableSelection::Cell(4, 0)
    );
    assert_eq!(
        v.full_selection(TableSelection::Row(3)),
        TableSelection::Row(3)
    );
    assert_eq!(
        v.visible_selection(TableSelection::Cell(4, 1)),
        None,
        "a hidden column"
    );
    assert_eq!(
        v.visible_selection(TableSelection::Cell(4, 3)),
        Some(TableSelection::Cell(4, 2))
    );
    assert!(v.hide(2));
    assert!(!v.hide(1), "the last visible column stays");
    assert!(v.any_hidden());
    assert!(v.show_all());
    assert!(!v.show_all());
    assert_eq!(v.data_col(3), Some(2));
    v.set_width(1, px(120.));
    assert_eq!((v.width(1), v.width(0)), (Some(px(120.)), None));
}

#[test]
fn fit_follows_the_content_past_the_starting_cap() {
    assert_eq!(fit_width("a", "", 100), px(736.));
    assert_eq!(fit_width("a", "", 200), px(800.));
    assert_eq!(fit_width("id", "", 2), px(49.));
}

#[test]
fn max_chars_scans_every_loaded_row() {
    let (columns, batches) = support::batches(
        "SELECT CASE WHEN x <= 1024 THEN 'ab' ELSE 'abcdefgh' END AS v FROM (WITH RECURSIVE r(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM r WHERE x < 2000) SELECT x FROM r)",
    );
    let mut s = ResultStore::new(columns);
    for b in batches {
        s.push(b);
    }
    assert_eq!(s.max_chars(0), 8);
}

#[gpui_kit::test]
fn a_hidden_column_leaves_the_grid_and_menus_still_address_the_right_cells(
    cx: &mut TestAppContext,
) {
    let o = opened(cx, T, "", 0);
    o.run(cx, "SELECT id, name, note FROM t ORDER BY id");
    o.idle(cx);
    let m = o.model(cx);
    assert!(m.update(cx, |m, cx| m.hide_column(0, 0, cx)));
    assert_eq!(headers(cx, &o), ["", "name", "note"]);
    assert!(!present(cx, o.win, ("result-col", 0usize)));
    cx.update_window(o.win, |_, w, cx| w.right_click("cell-0-2", cx))
        .unwrap();
    cx.run_until_parked();
    cx.update_window(o.win, |_, w, cx| w.click("grid-menu-copy", cx))
        .unwrap();
    cx.run_until_parked();
    assert_eq!(
        cx.read_from_clipboard().and_then(|c| c.text()).as_deref(),
        Some("x")
    );
    o.run(cx, "SELECT id, name, note FROM t ORDER BY id DESC");
    o.idle(cx);
    assert_eq!(
        headers(cx, &o),
        ["", "name", "note"],
        "same columns keep the view"
    );
    o.run(cx, "SELECT id, note FROM t");
    o.idle(cx);
    assert_eq!(headers(cx, &o), ["", "id", "note"], "other columns drop it");
    o.run(cx, "SELECT id, name, note FROM t ORDER BY id");
    o.idle(cx);
    assert_eq!(
        headers(cx, &o),
        ["", "id", "name", "note"],
        "a dropped view stays dropped"
    );
    assert!(m.update(cx, |m, cx| m.hide_column(0, 2, cx)));
    assert!(m.update(cx, |m, cx| m.show_all_columns(0, cx)));
    assert_eq!(headers(cx, &o), ["", "id", "name", "note"]);
}

#[gpui_kit::test]
fn an_edit_right_of_a_hidden_column_stages_its_own_column(cx: &mut TestAppContext) {
    let o = launched(cx, T, "");
    o.open_relation(cx, "t", RelationKind::Table);
    o.wait_browse(cx, "SELECT *\nFROM main.t\nORDER BY id ASC");
    let m = o.model(cx);
    assert!(m.update(cx, |m, cx| m.hide_column(0, 1, cx)));
    cx.run_until_parked();
    cx.update_window(o.win, |_, w, cx| w.double_click("cell-0-2", cx))
        .unwrap();
    cx.run_until_parked();
    assert_eq!(pane(cx, &o).read_with(cx, |p, _| p.editing()), Some((0, 2)));
    cx.update_window(o.win, |_, w, cx| {
        w.input("q", cx);
        w.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    m.read_with(cx, |m, _| {
        assert_eq!(m.edits().cell(0, 2), Some(&EditValue::Text("q".into())));
        assert_eq!(m.edits().cell(0, 1), None, "the hidden column is untouched");
    });
}

#[gpui_kit::test]
fn fit_sizes_a_column_to_its_loaded_cells(cx: &mut TestAppContext) {
    let o = opened(cx, T, "", 0);
    o.run(cx, "SELECT id, hex(zeroblob(60)) AS wide FROM t");
    o.idle(cx);
    let m = o.model(cx);
    let table = pane(cx, &o).read_with(cx, |p, _| p.table().clone());
    let width =
        |cx: &mut TestAppContext| table.read_with(cx, |t, cx| t.delegate().column(2, cx).width);
    assert_eq!(width(cx), px(400.), "the starting cap");
    assert!(m.update(cx, |m, cx| m.fit_column(0, 1, cx)));
    cx.run_until_parked();
    assert_eq!(width(cx), px(800.));
}

#[gpui_kit::test]
fn a_drag_right_of_a_hidden_column_sizes_its_own_column(cx: &mut TestAppContext) {
    let o = opened(cx, T, "", 0);
    o.run(cx, "SELECT id, name, note FROM t");
    o.idle(cx);
    let m = o.model(cx);
    assert!(m.update(cx, |m, cx| m.hide_column(0, 0, cx)));
    let table = pane(cx, &o).read_with(cx, |p, _| p.table().clone());
    cx.run_until_parked();
    // The kit reports every visible width at drag end, gutter first; only "note" moved.
    table.update(cx, |t, cx| {
        let mut widths: Vec<_> = (0..3).map(|ix| t.delegate().column(ix, cx).width).collect();
        widths[2] = px(222.);
        cx.emit(TableEvent::ColumnWidthsChanged(widths));
    });
    cx.run_until_parked();
    m.read_with(cx, |m, _| {
        let view = m.column_view(0).unwrap();
        assert_eq!(
            (view.width(2), view.width(1), view.width(0)),
            (Some(px(222.)), None, None)
        );
    });
    assert_eq!(
        table.read_with(cx, |t, cx| t.delegate().column(2, cx).width),
        px(222.)
    );
}
