//! The record view and the inspector show a staged field's loaded value with a per-field
//! revert, and copy a capped original whole.
mod support;
use dbento_app::record::{Was, record_fields};
use dbento_app::results_pane::ResultsPane;
use dbento_core::{CellRef, EditValue, RelationKind};
use gpui_kit::component::table::TableSelection;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, ElementId, Entity, Focusable as _, TestAppContext};
use support::{
    Opened, click_id, fixture, label_of, launched, present, ready, wait_idle, wait_prompt,
};

const T: &str = "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT, note TEXT); \
                 INSERT INTO t VALUES (1, 'b', ' pad '), (2, 'a', NULL), \
                 (3, 'c', substr(replace(hex(zeroblob(300)), '0', 'x'), 1, 300));";
const T_SQL: &str = "SELECT *\nFROM main.t\nORDER BY id ASC";

fn browsed(cx: &mut TestAppContext) -> Opened {
    let o = launched(cx, T, "");
    o.open_relation(cx, "t", RelationKind::Table);
    o.wait_browse(cx, T_SQL);
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

#[gpui_kit::test]
fn record_fields_carry_the_was_line_of_staged_cells_only(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = ready(cx, &fx, "t");
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 1, "z", cx)));
    assert!(m.update(cx, |m, cx| m.stage_cell(2, 2, "short", cx)));
    m.read_with(cx, |m, _| {
        let f = record_fields(m, 0, 0);
        assert_eq!(
            f[1].was,
            Some(Was {
                text: "was: b".into(),
                copy: None
            })
        );
        assert_eq!(f[0].was, None);
        assert_eq!(f[2].was, None);
        let long = record_fields(m, 0, 2);
        assert_eq!(
            long[2].was,
            Some(Was {
                text: format!("was: {}\u{2026}", "x".repeat(200)),
                copy: Some("x".repeat(300))
            })
        );
    });
}

#[gpui_kit::test]
fn the_inspector_shows_was_under_a_staged_field_and_reverts_it(cx: &mut TestAppContext) {
    let o = browsed(cx);
    click_id(cx, o.win, "inspector-toggle");
    select(cx, &o, 0);
    let m = o.model(cx);
    assert!(!present(cx, o.win, ("inspector-was", 1usize)));
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 1, "z", cx)));
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 2, "q", cx)));
    assert_eq!(
        label_of(cx, o.win, ("inspector-was", 1usize)).as_deref(),
        Some("was: b")
    );
    assert_eq!(
        label_of(cx, o.win, ("inspector-was", 2usize)).as_deref(),
        Some("was: \" pad \"")
    );
    assert!(!present(cx, o.win, ("inspector-was", 0usize)));
    assert!(!present(cx, o.win, ("inspector-was-copy", 1usize)));
    click(cx, &o, ("inspector-revert", 1usize));
    m.read_with(cx, |m, _| {
        assert_eq!(m.edits().cell(0, 1), None);
        assert_eq!(m.edits().cell(0, 2), Some(&EditValue::Text("q".into())));
    });
    assert!(!present(cx, o.win, ("inspector-was", 1usize)));
    assert!(m.update(cx, |m, cx| m.undo(cx)));
    assert_eq!(
        label_of(cx, o.win, ("inspector-was", 1usize)).as_deref(),
        Some("was: b"),
        "the revert was one undo step"
    );
}

#[gpui_kit::test]
fn the_record_view_copies_a_capped_original_whole(cx: &mut TestAppContext) {
    let o = browsed(cx);
    select(cx, &o, 2);
    click(cx, &o, "row-view-expand");
    let m = o.model(cx);
    assert!(m.update(cx, |m, cx| m.stage_cell(2, 2, "short", cx)));
    assert_eq!(
        label_of(cx, o.win, ("record-was", 2usize)),
        Some(format!("was: {}\u{2026}", "x".repeat(200)))
    );
    click(cx, &o, ("record-was-copy", 2usize));
    assert_eq!(
        cx.read_from_clipboard().and_then(|c| c.text()),
        Some("x".repeat(300))
    );
}

#[gpui_kit::test]
fn revert_discards_the_fields_open_draft(cx: &mut TestAppContext) {
    let o = browsed(cx);
    click(cx, &o, "row-view-expand");
    let m = o.model(cx);
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 1, "z", cx)));
    click(cx, &o, ("record-value", 1usize));
    assert!(present(cx, o.win, "record-editor"));
    cx.update_window(o.win, |_, w, cx| w.input("q", cx))
        .unwrap();
    cx.run_until_parked();
    click(cx, &o, ("record-revert", 1usize));
    assert!(!present(cx, o.win, "record-editor"));
    cx.update_window(o.win, |_, w, cx| w.press("ctrl-enter", cx))
        .unwrap();
    cx.run_until_parked();
    m.read_with(cx, |m, _| assert_eq!(m.edits().cell(0, 1), None));
}

#[test]
fn capped_bytes_offer_their_whole_value_too() {
    let ten: Vec<u8> = (1..=10).collect();
    assert_eq!(
        Was::of(CellRef::Bytes(&ten)).copy.as_deref(),
        Some("\\x0102030405060708090a")
    );
    assert_eq!(
        Was::of(CellRef::Bytes(&[1, 2, 3, 4, 5, 6, 7, 8])).copy,
        None
    );
}

#[gpui_kit::test]
fn revert_is_off_while_a_save_is_in_flight(cx: &mut TestAppContext) {
    let o = browsed(cx);
    click(cx, &o, "row-view-expand");
    let m = o.model(cx);
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 1, "z", cx)));
    let form = pane(cx, &o).read_with(cx, |p, _| p.record_form().clone());
    assert!(form.read_with(cx, |f, cx| f.revert_live(cx)));
    assert!(m.update(cx, |m, cx| m.save(cx)));
    wait_prompt(cx, &m);
    assert!(present(cx, o.win, ("record-revert", 1usize)));
    assert!(!form.read_with(cx, |f, cx| f.revert_live(cx)));
    m.update(cx, |m, cx| m.rollback(cx));
    wait_idle(cx, &m);
    assert!(form.read_with(cx, |f, cx| f.revert_live(cx)));
}

#[gpui_kit::test]
fn the_grid_keeps_the_cell_through_the_inspector_and_the_record_view(cx: &mut TestAppContext) {
    let o = browsed(cx);
    let m = o.model(cx);
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 1, "z", cx)));
    // Table column 2 is data column 1, past the gutter.
    let table = pane(cx, &o).read_with(cx, |p, _| p.table().clone());
    cx.update_window(o.win, |_, w, cx| {
        table.update(cx, |t, cx| t.set_selected_cell(0, 2, cx));
        table.read(cx).focus_handle(cx).focus(w, cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert!(present(cx, o.win, "status-was"));
    click_id(cx, o.win, "inspector-toggle");
    assert_eq!(
        table.read_with(cx, |t, _| t.selection()),
        TableSelection::Cell(0, 2)
    );
    assert!(present(cx, o.win, "status-was"), "opening the inspector");
    click_id(cx, o.win, "inspector-toggle");
    click(cx, &o, "row-view-expand");
    click(cx, &o, "row-view-expand");
    assert_eq!(
        table.read_with(cx, |t, _| t.selection()),
        TableSelection::Cell(0, 2)
    );
    assert!(
        present(cx, o.win, "status-was"),
        "back from the record view"
    );
}
