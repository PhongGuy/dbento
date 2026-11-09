//! The tab menu: close, close others, close all, rename, duplicate and save as query.
use std::path::PathBuf;

use crate::support::{Opened, click_id, label_of, launched, opened, present, wait_real};
use dbento_core::RelationKind;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext};

fn tab_menu(cx: &mut TestAppContext, o: &Opened, ix: usize) {
    cx.update_window(o.win, |_, w, cx| w.right_click(("tab", ix), cx))
        .unwrap();
    cx.run_until_parked();
}
fn set(cx: &mut TestAppContext, o: &Opened, ix: usize, sql: &str) {
    let tab = o.shell.read_with(cx, |s, _| s.tabs()[ix].clone());
    cx.update_window(o.win, |_, w, cx| {
        tab.update(cx, |t, cx| t.set_sql(sql, w, cx))
    })
    .unwrap();
}
fn sql_of(cx: &mut TestAppContext, o: &Opened, ix: usize) -> String {
    o.shell.read_with(cx, |s, cx| s.tabs()[ix].read(cx).sql(cx))
}
fn count(cx: &mut TestAppContext, o: &Opened) -> usize {
    o.shell.read_with(cx, |s, _| s.tabs().len())
}
fn type_name(cx: &mut TestAppContext, o: &Opened, text: &str) {
    wait_real(cx, "the name dialog", |cx| present(cx, o.win, "save-name"));
    cx.update_window(o.win, |_, w, cx| {
        w.press("ctrl-a", cx);
        w.input(text, cx);
    })
    .unwrap();
    click_id(cx, o.win, "save-ok");
}

#[gpui_kit::test]
fn the_tab_menu_lists_its_items(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    tab_menu(cx, &o, 0);
    for id in [
        "tab-menu-close",
        "tab-menu-close-others",
        "tab-menu-close-all",
        "tab-menu-rename",
        "tab-menu-duplicate",
        "tab-menu-save",
    ] {
        assert!(present(cx, o.win, id), "{id}");
    }
    click_id(cx, o.win, "tab-menu-close");
    assert_eq!(count(cx, &o), 0);
}

#[gpui_kit::test]
fn close_others_keeps_the_clicked_tab(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    click_id(cx, o.win, "tab-new");
    click_id(cx, o.win, "tab-new");
    set(cx, &o, 1, "SELECT 1");
    tab_menu(cx, &o, 1);
    click_id(cx, o.win, "tab-menu-close-others");
    assert!(
        !present(cx, o.win, "confirm-dialog"),
        "the closing tabs are empty"
    );
    assert_eq!(count(cx, &o), 1);
    assert_eq!(sql_of(cx, &o, 0), "SELECT 1");
}

#[gpui_kit::test]
fn close_all_asks_once_for_unsaved_sql(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    click_id(cx, o.win, "tab-new");
    set(cx, &o, 0, "SELECT 42");
    set(cx, &o, 1, "SELECT 43");
    for (answer, left) in [("discard-keep", 2), ("discard-close", 0)] {
        tab_menu(cx, &o, 0);
        click_id(cx, o.win, "tab-menu-close-all");
        wait_real(cx, "the question", |cx| {
            present(cx, o.win, "confirm-dialog")
        });
        click_id(cx, o.win, answer);
        assert!(!present(cx, o.win, "confirm-dialog"), "one question only");
        assert_eq!(count(cx, &o), left, "{answer}");
    }
}

#[gpui_kit::test]
fn rename_labels_the_tab_and_a_file_tab_cannot_be_renamed(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    tab_menu(cx, &o, 0);
    click_id(cx, o.win, "tab-menu-rename");
    type_name(cx, &o, "orders");
    assert_eq!(
        label_of(cx, o.win, ("tab", 0usize)).as_deref(),
        Some("orders")
    );
    let tab = o.shell.read_with(cx, |s, _| s.tabs()[0].clone());
    tab.update(cx, |t, cx| {
        t.set_file(PathBuf::from("queries/lite/a.sql"), String::new(), cx)
    });
    cx.run_until_parked();
    tab_menu(cx, &o, 0);
    click_id(cx, o.win, "tab-menu-rename");
    assert!(!present(cx, o.win, "save-name"));
    assert_eq!(
        label_of(cx, o.win, ("tab", 0usize)).as_deref(),
        Some("a.sql")
    );
}

#[gpui_kit::test]
fn duplicate_opens_the_same_sql_on_the_same_connection(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    set(cx, &o, 0, "SELECT 7");
    tab_menu(cx, &o, 0);
    click_id(cx, o.win, "tab-menu-duplicate");
    assert_eq!(count(cx, &o), 2);
    assert_eq!(o.shell.read_with(cx, |s, _| s.active()), Some(1));
    assert_eq!(sql_of(cx, &o, 1), "SELECT 7");
    assert_eq!(
        label_of(cx, o.win, ("tab-connection", 1usize)).as_deref(),
        Some("lite")
    );
    assert!(
        o.model(cx).read_with(cx, |m, _| m.results().is_empty()),
        "never runs"
    );
}

#[gpui_kit::test]
fn save_as_query_asks_for_a_name_for_that_tab(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    set(cx, &o, 0, "SELECT 9");
    click_id(cx, o.win, "tab-new");
    tab_menu(cx, &o, 0);
    click_id(cx, o.win, "tab-menu-save");
    wait_real(cx, "the name dialog", |cx| present(cx, o.win, "save-name"));
    assert_eq!(o.shell.read_with(cx, |s, _| s.active()), Some(0));
}

#[gpui_kit::test]
fn the_tab_menu_counts_as_open_until_dismissed(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    assert!(!o.shell.read_with(cx, |s, _| s.tab_menu_open()));
    tab_menu(cx, &o, 0);
    assert!(o.shell.read_with(cx, |s, _| s.tab_menu_open()));
    cx.update_window(o.win, |_, w, cx| w.press("escape", cx))
        .unwrap();
    cx.run_until_parked();
    assert!(!o.shell.read_with(cx, |s, _| s.tab_menu_open()));
}

#[gpui_kit::test]
fn close_others_speaks_of_one_tab_when_one_closes(cx: &mut TestAppContext) {
    let o = launched(
        cx,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT); INSERT INTO t VALUES (1, 'b');",
        "",
    );
    o.open_relation(cx, "t", RelationKind::Table);
    o.wait_browse(cx, "SELECT *\nFROM main.t\nORDER BY id ASC");
    assert!(o.model(cx).update(cx, |m, cx| m.stage_cell(0, 1, "z", cx)));
    click_id(cx, o.win, "tab-new");
    tab_menu(cx, &o, 1);
    click_id(cx, o.win, "tab-menu-close-others");
    wait_real(cx, "the question", |cx| {
        present(cx, o.win, "confirm-dialog")
    });
    assert_eq!(
        label_of(cx, o.win, "confirm-detail").as_deref(),
        Some("Closing the tab drops them.")
    );
}

#[gpui_kit::test]
fn rename_trims_the_name_and_refuses_a_blank_one(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    tab_menu(cx, &o, 0);
    click_id(cx, o.win, "tab-menu-rename");
    type_name(cx, &o, "   ");
    assert!(
        present(cx, o.win, "save-name-error"),
        "a blank name is refused"
    );
    type_name(cx, &o, "  orders  ");
    assert_eq!(
        label_of(cx, o.win, ("tab", 0usize)).as_deref(),
        Some("orders")
    );
}
