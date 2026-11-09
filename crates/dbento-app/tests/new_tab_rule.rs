//! `+` and Ctrl+T follow a changed explorer selection; a hovered connection row offers `+`.
use crate::support;
use crate::support::{Opened, click_id, opened, present};
use dbento_app::explorer::{connection_id, part_id};
use dbento_app::shell::new_tab_connection;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext};

fn active_connection(cx: &mut TestAppContext, o: &Opened) -> String {
    o.model(cx)
        .read_with(cx, |m, _| m.spec().connection.clone())
}
fn count(cx: &mut TestAppContext, o: &Opened) -> usize {
    o.shell.read_with(cx, |s, _| s.tabs().len())
}

#[test]
fn a_moved_selection_wins_else_the_active_tab() {
    assert_eq!(new_tab_connection(Some(0), Some(1), true), Some(1));
    assert_eq!(new_tab_connection(Some(0), Some(1), false), Some(0));
    assert_eq!(new_tab_connection(Some(0), None, true), Some(0));
    assert_eq!(new_tab_connection(None, Some(1), false), Some(1));
    assert_eq!(new_tab_connection(None, None, true), None);
}

#[gpui_kit::test]
fn tab_new_and_ctrl_t_follow_a_selection_made_after_activation(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    click_id(cx, o.win, connection_id("nopg"));
    click_id(cx, o.win, "tab-new");
    assert_eq!(
        (count(cx, &o), active_connection(cx, &o).as_str()),
        (2, "nopg")
    );
    click_id(cx, o.win, ("tab", 0usize));
    cx.update_window(o.win, |_, w, cx| w.press("ctrl-t", cx))
        .unwrap();
    cx.run_until_parked();
    assert_eq!(
        (count(cx, &o), active_connection(cx, &o).as_str()),
        (3, "lite"),
        "activating a tab re-syncs the selection"
    );
}

#[gpui_kit::test]
fn a_hovered_connection_row_offers_a_new_query(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let plus = |name: &str| part_id("connection-new-query", name);
    assert!(!present(cx, o.win, plus("nopg")));
    support::hover(cx, o.win, connection_id("nopg"));
    assert!(present(cx, o.win, plus("nopg")));
    assert!(!present(cx, o.win, plus("lite")));
    click_id(cx, o.win, plus("nopg"));
    assert_eq!(
        (count(cx, &o), active_connection(cx, &o).as_str()),
        (2, "nopg")
    );
    let sql = o
        .shell
        .read_with(cx, |s, cx| s.tab().unwrap().read(cx).sql(cx));
    assert_eq!(sql, "");
}
