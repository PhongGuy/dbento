//! The row inspector's dock: one slot shared with history, a key, a saved flag, the header.
use crate::support;
use crate::support::{Opened, click_id, label_of, launched, opened, present};
use dbento_app::app::{AppPaths, SavedState, start};
use dbento_app::persist::{load_json, save_json};
use dbento_app::results_pane::ResultsPane;
use dbento_core::RelationKind;
use gpui_kit::test::TestWindowExt as _;
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
fn opening_one_dock_closes_the_other(cx: &mut TestAppContext) {
    let o = opened(cx, T, "", 0);
    o.run(cx, "SELECT * FROM b");
    o.idle(cx);
    click_id(cx, o.win, "inspector-toggle");
    assert!(o.shell.read_with(cx, |s, _| s.inspector_open()));
    assert!(present(cx, o.win, "inspector-panel"));
    click_id(cx, o.win, "history-toggle");
    assert!(
        o.shell
            .read_with(cx, |s, _| s.history_open() && !s.inspector_open())
    );
    assert!(present(cx, o.win, "history-panel"));
    assert!(!present(cx, o.win, "inspector-panel"));
    cx.update_window(o.win, |_, w, cx| w.press("ctrl-shift-d", cx))
        .unwrap();
    cx.run_until_parked();
    assert!(
        o.shell
            .read_with(cx, |s, _| s.inspector_open() && !s.history_open())
    );
    assert!(!present(cx, o.win, "history-panel"));
    let saved: SavedState = load_json(&AppPaths::in_dir(o.files.path()).state).value;
    assert!(saved.inspector_open && !saved.history_open);
    click_id(cx, o.win, "inspector-close");
    assert!(!o.shell.read_with(cx, |s, _| s.inspector_open()));
}

#[gpui_kit::test]
fn an_open_inspector_comes_back_open(cx: &mut TestAppContext) {
    support::init(cx);
    let (fx, files) = (support::fixture("", ""), tempfile::tempdir().unwrap());
    let state = SavedState {
        inspector_open: true,
        ..SavedState::default()
    };
    save_json(&AppPaths::in_dir(files.path()).state, &state).unwrap();
    let (win, _) = cx
        .update(|cx| {
            start(
                support::services(files.path()),
                Some(fx.dir.path().into()),
                cx,
            )
        })
        .unwrap();
    assert!(present(cx, win, "inspector-panel"));
    assert!(present(cx, win, "inspector-empty"));
}

#[gpui_kit::test]
fn the_header_names_the_row_table_and_identity(cx: &mut TestAppContext) {
    let o = launched(cx, T, "");
    o.open_relation(cx, "b", RelationKind::Table);
    o.wait_browse(cx, "SELECT *\nFROM main.b\nORDER BY id ASC");
    click_id(cx, o.win, "inspector-toggle");
    assert!(present(cx, o.win, "inspector-empty"));
    select(cx, &o, 1);
    assert_eq!(
        label_of(cx, o.win, "inspector-title").as_deref(),
        Some("Row 2")
    );
    assert_eq!(
        label_of(cx, o.win, "inspector-table").as_deref(),
        Some("main.b")
    );
    assert_eq!(
        label_of(cx, o.win, "inspector-identity").as_deref(),
        Some("id = 2")
    );
    o.run(cx, "SELECT 1 AS n");
    o.idle(cx);
    select(cx, &o, 0);
    assert_eq!(
        label_of(cx, o.win, "inspector-title").as_deref(),
        Some("Row 1")
    );
    assert!(!present(cx, o.win, "inspector-table"));
    assert!(!present(cx, o.win, "inspector-identity"));
}

#[gpui_kit::test]
fn the_header_follows_the_active_tab(cx: &mut TestAppContext) {
    let o = launched(cx, T, "");
    o.open_relation(cx, "b", RelationKind::Table);
    o.wait_browse(cx, "SELECT *\nFROM main.b\nORDER BY id ASC");
    click_id(cx, o.win, "inspector-toggle");
    select(cx, &o, 1);
    let shell = o.shell.clone();
    cx.update_window(o.win, |_, w, cx| shell.update(cx, |s, cx| s.new_tab(w, cx)))
        .unwrap();
    assert!(present(cx, o.win, "inspector-empty"));
    cx.update_window(o.win, |_, w, cx| {
        shell.update(cx, |s, cx| s.activate(0, w, cx))
    })
    .unwrap();
    assert_eq!(
        label_of(cx, o.win, "inspector-title").as_deref(),
        Some("Row 2")
    );
    // A selection on the tab followed again re-renders the header.
    select(cx, &o, 0);
    assert_eq!(
        label_of(cx, o.win, "inspector-title").as_deref(),
        Some("Row 1")
    );
}
