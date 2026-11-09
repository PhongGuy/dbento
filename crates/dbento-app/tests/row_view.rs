//! The inspector is the one row view: its toggle sits in the result footer and Record is its
//! expand mode, which takes the dock's place.
use crate::support::{Opened, click_id, label_of, launched, opened, present};
use dbento_app::results_pane::{ResultView, ResultsPane};
use dbento_core::RelationKind;
use gpui_kit::{AppContext as _, Entity, Focusable as _, TestAppContext};

const T: &str = "CREATE TABLE b(id INTEGER PRIMARY KEY, note TEXT); \
                 INSERT INTO b VALUES (1, 'n'), (2, 'm');";

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

#[gpui_kit::test]
fn the_toggle_lives_in_the_result_footer(cx: &mut TestAppContext) {
    let o = opened(cx, T, "", 0);
    assert!(
        !present(cx, o.win, "inspector-toggle"),
        "no result, no footer, no toggle"
    );
    o.run(cx, "SELECT * FROM b");
    o.idle(cx);
    assert!(present(cx, o.win, "inspector-toggle"));
    assert!(present(cx, o.win, "row-view-expand"));
    assert!(!present(cx, o.win, "view-grid") && !present(cx, o.win, "view-record"));
    assert!(!pane(cx, &o).read_with(cx, |p, _| p.inspector_open()));
    click_id(cx, o.win, "inspector-toggle");
    assert!(o.shell.read_with(cx, |s, _| s.inspector_open()));
    assert!(present(cx, o.win, "inspector-panel"));
    assert!(
        pane(cx, &o).read_with(cx, |p, _| p.inspector_open()),
        "the toggle shows the dock's state"
    );
}

#[gpui_kit::test]
fn expand_takes_the_docks_place_and_collapse_gives_it_back(cx: &mut TestAppContext) {
    let o = launched(cx, T, "");
    o.open_relation(cx, "b", RelationKind::Table);
    o.wait_browse(cx, "SELECT *\nFROM main.b\nORDER BY id ASC");
    click_id(cx, o.win, "inspector-toggle");
    select(cx, &o, 1);
    assert!(present(cx, o.win, "inspector-panel"));
    click_id(cx, o.win, "row-view-expand");
    assert_eq!(
        pane(cx, &o).read_with(cx, |p, _| p.view()),
        ResultView::Record
    );
    assert!(present(cx, o.win, "record-view"));
    assert!(!present(cx, o.win, "inspector-panel"));
    assert!(
        o.shell.read_with(cx, |s, _| s.inspector_open()),
        "the dock is only hidden"
    );
    assert_eq!(
        label_of(cx, o.win, "record-position").as_deref(),
        Some("Row 2 of 2")
    );
    click_id(cx, o.win, "row-view-expand");
    assert_eq!(
        pane(cx, &o).read_with(cx, |p, _| p.view()),
        ResultView::Grid
    );
    assert!(present(cx, o.win, "inspector-panel"));
    assert!(present(cx, o.win, "result-grid"));
}
