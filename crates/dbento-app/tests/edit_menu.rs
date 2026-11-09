//! The grid's context menu: right-click targets and what the items do.
use crate::support::{Opened, label_of, launched, present};
use dbento_app::keymap::{DELETE_ROW_KEYS, EDIT_CELL_KEYS, chip_keys};
use dbento_app::results_pane::ResultsPane;
use dbento_app::staged::GridRow::Loaded;
use dbento_app::tab_model::CopyKind;
use dbento_core::{EditValue, RelationKind};
use gpui_kit::component::table::TableSelection;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    AppContext as _, Entity, InputEvent as _, MouseButton, MouseDownEvent, MouseUpEvent,
    TestAppContext, point, px,
};

const T: &str = "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT NOT NULL, note TEXT); \
                 INSERT INTO t VALUES (1, 'b', 'x'), (2, 'a', NULL); \
                 CREATE TABLE e(id INTEGER PRIMARY KEY, v TEXT);";

fn browse(cx: &mut TestAppContext, table: &str) -> Opened {
    let o = launched(cx, T, "");
    o.open_relation(cx, table, RelationKind::Table);
    o.wait_browse(cx, &format!("SELECT *\nFROM main.{table}\nORDER BY id ASC"));
    o
}
fn pane(cx: &mut TestAppContext, o: &Opened) -> Entity<ResultsPane> {
    o.shell
        .read_with(cx, |s, cx| s.tab().unwrap().read(cx).results_pane().clone())
}
fn selection(cx: &mut TestAppContext, o: &Opened) -> TableSelection {
    let table = pane(cx, o).read_with(cx, |p, _| p.table().clone());
    table.read_with(cx, |t, _| t.selection())
}
fn right_click(cx: &mut TestAppContext, o: &Opened, id: &'static str) {
    cx.update_window(o.win, |_, w, cx| w.right_click(id, cx))
        .unwrap();
    cx.run_until_parked();
}
fn click(cx: &mut TestAppContext, o: &Opened, id: &'static str) {
    cx.update_window(o.win, |_, w, cx| w.click(id, cx)).unwrap();
    cx.run_until_parked();
}

#[gpui_kit::test]
fn right_click_selects_the_cell_and_set_null_stages(cx: &mut TestAppContext) {
    let o = browse(cx, "t");
    right_click(cx, &o, "cell-0-2");
    assert_eq!(selection(cx, &o), TableSelection::Cell(0, 3));
    assert!(present(cx, o.win, "grid-menu-set-null"));
    click(cx, &o, "grid-menu-set-null");
    assert_eq!(
        o.model(cx)
            .read_with(cx, |m, _| m.edits().cell(0, 2).cloned()),
        Some(EditValue::Null)
    );
    assert_eq!(label_of(cx, o.win, "cell-0-2").as_deref(), Some("NULL"));
    assert!(!present(cx, o.win, "grid-menu-set-null"), "the menu closes");
}

#[gpui_kit::test]
fn duplicate_and_add_put_rows_on_top_and_select_them(cx: &mut TestAppContext) {
    let o = browse(cx, "t");
    right_click(cx, &o, "cell-1-1");
    click(cx, &o, "grid-menu-duplicate-row");
    assert!(present(cx, o.win, "row-inserted-0"));
    assert_eq!(label_of(cx, o.win, "cell-0-1").as_deref(), Some("a"));
    assert_eq!(selection(cx, &o), TableSelection::Cell(0, 1));
    right_click(cx, &o, "cell-2-1");
    click(cx, &o, "grid-menu-add-row");
    assert!(present(cx, o.win, "row-inserted-1"));
    assert_eq!(label_of(cx, o.win, "cell-1-1").as_deref(), Some("DEFAULT"));
    assert_eq!(selection(cx, &o), TableSelection::Cell(1, 1));
}

#[gpui_kit::test]
fn edit_opens_the_editor_and_revert_row_unstages(cx: &mut TestAppContext) {
    let o = browse(cx, "t");
    right_click(cx, &o, "cell-0-1");
    click(cx, &o, "grid-menu-edit");
    assert_eq!(pane(cx, &o).read_with(cx, |p, _| p.editing()), Some((0, 1)));
    cx.update_window(o.win, |_, w, cx| {
        w.input("z", cx);
        w.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(label_of(cx, o.win, "cell-0-1").as_deref(), Some("z"));
    right_click(cx, &o, "cell-0-1");
    click(cx, &o, "grid-menu-revert-row");
    assert!(o.model(cx).read_with(cx, |m, _| m.edits().is_empty()));
    assert_eq!(label_of(cx, o.win, "cell-0-1").as_deref(), Some("b"));
}

#[gpui_kit::test]
fn copy_as_update_puts_the_sql_on_the_clipboard(cx: &mut TestAppContext) {
    let o = browse(cx, "t");
    right_click(cx, &o, "cell-0-1");
    click(cx, &o, "grid-menu-copy-update");
    let want = o
        .model(cx)
        .read_with(cx, |m, _| m.copy_query(Loaded(0), CopyKind::Update));
    assert!(want.is_some());
    assert_eq!(cx.read_from_clipboard().and_then(|c| c.text()), want);
}

#[gpui_kit::test]
fn right_click_on_an_empty_table_adds_a_row(cx: &mut TestAppContext) {
    let o = browse(cx, "e");
    right_click(cx, &o, "result-grid");
    click(cx, &o, "grid-menu-add-row");
    assert!(present(cx, o.win, "row-inserted-0"));
    assert_eq!(o.model(cx).read_with(cx, |m, _| m.edits().inserted()), 1);
}

#[gpui_kit::test]
fn a_right_click_on_a_cells_padding_targets_that_cell(cx: &mut TestAppContext) {
    let o = browse(cx, "t");
    cx.update_window(o.win, |_, w, cx| {
        w.render_frame(cx);
        let cell = w.find("cell-0-2").bounds();
        // Inside the td's 8 px left padding, outside the text element.
        let at = point(cell.origin.x - px(4.), cell.center().y);
        let down = MouseDownEvent {
            button: MouseButton::Right,
            position: at,
            modifiers: Default::default(),
            click_count: 1,
            first_mouse: false,
        };
        w.dispatch_event(down.to_platform_input(), cx);
        let up = MouseUpEvent {
            button: MouseButton::Right,
            position: at,
            modifiers: Default::default(),
            click_count: 1,
        };
        w.dispatch_event(up.to_platform_input(), cx);
        w.render_frame(cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(selection(cx, &o), TableSelection::Cell(0, 3));
    assert!(present(cx, o.win, "grid-menu-set-null"));
}

#[gpui_kit::test]
fn a_right_click_below_the_rows_forgets_the_last_cell(cx: &mut TestAppContext) {
    let o = browse(cx, "t");
    right_click(cx, &o, "cell-0-2");
    assert!(present(cx, o.win, "grid-menu-set-null"));
    right_click(cx, &o, "result-grid");
    assert!(!present(cx, o.win, "grid-menu-set-null"));
    assert!(present(cx, o.win, "grid-menu-add-row"));
}

#[gpui_kit::test]
fn an_item_does_nothing_once_the_rows_have_moved(cx: &mut TestAppContext) {
    let o = browse(cx, "t");
    right_click(cx, &o, "cell-0-1");
    // A row added with the menu open moves loaded row 0 down; the menu's row is gone.
    o.model(cx).update(cx, |m, cx| m.add_row(cx));
    cx.run_until_parked();
    click(cx, &o, "grid-menu-delete-row");
    o.model(cx).read_with(cx, |m, _| {
        assert_eq!(m.edits().inserted(), 1, "the new row stays");
        assert!(!m.edits().is_deleted(0) && !m.edits().is_deleted(1));
    });
}

#[gpui_kit::test]
fn grid_menu_items_show_their_keys(cx: &mut TestAppContext) {
    let o = browse(cx, "t");
    right_click(cx, &o, "cell-0-1");
    assert_eq!(
        label_of(cx, o.win, "grid-menu-edit-keys"),
        Some(chip_keys(EDIT_CELL_KEYS[0]))
    );
    assert_eq!(
        label_of(cx, o.win, "grid-menu-delete-row-keys"),
        Some(chip_keys(DELETE_ROW_KEYS))
    );
    assert!(!present(cx, o.win, "grid-menu-set-null-keys"));
}
