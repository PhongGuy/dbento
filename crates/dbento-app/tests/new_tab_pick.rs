//! New tab with no tab open and nothing selected in the explorer: it asks which connection
//! to open on, or offers to make one when the workspace has none.
use crate::derived_tabs::tab_conns;
use crate::support::{Started, click_id, label_of, present, started, wait_real};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, Focusable as _, TestAppContext};

const TWO: &str = "dbento = 1\n[connections.lite]\nengine = \"sqlite\"\npath = \"app.db\"\n[connections.rw]\nengine = \"sqlite\"\npath = \"app.db\"\n";

fn ctrl_t(cx: &mut TestAppContext, s: &Started) {
    cx.update_window(s.win, |_, w, cx| w.press("ctrl-t", cx))
        .unwrap();
    cx.run_until_parked();
}

fn editor_focused(cx: &mut TestAppContext, s: &Started) -> bool {
    let editor = s
        .shell
        .read_with(cx, |sh, cx| sh.tab().unwrap().read(cx).editor().clone());
    cx.update_window(s.win, |_, w, cx| {
        editor.read(cx).focus_handle(cx).is_focused(w)
    })
    .unwrap()
}

// The picker names what it is for; no warning notice, since nothing went wrong.
fn assert_asks(cx: &mut TestAppContext, s: &Started) {
    assert!(tab_conns(cx, s).is_empty(), "nothing opens before the pick");
    assert_eq!(
        label_of(cx, s.win, "pick-connection-dialog").as_deref(),
        Some("New tab"),
        "the picker is titled for what it opens"
    );
    assert_eq!(
        label_of(cx, s.win, "pick-detail").as_deref(),
        Some("Pick a connection for the new tab."),
        "the detail says what to do"
    );
    assert!(
        !present(cx, s.win, "confirm-sql"),
        "there is no SQL to show"
    );
    assert!(!present(cx, s.win, "notice"), "no warning notice");
}

#[gpui_kit::test]
fn ctrl_t_with_nothing_selected_opens_the_picked_connection_focused(cx: &mut TestAppContext) {
    let s = started(cx, TWO);
    ctrl_t(cx, &s);
    assert_asks(cx, &s);
    click_id(cx, s.win, ("pick-connection", 1usize));
    assert!(!present(cx, s.win, "pick-connection-dialog"));
    assert_eq!(tab_conns(cx, &s), ["rw"]);
    assert!(editor_focused(cx, &s), "the new tab's editor takes focus");
}

#[gpui_kit::test]
fn the_start_card_new_tab_asks_the_same_and_cancel_opens_nothing(cx: &mut TestAppContext) {
    let s = started(cx, TWO);
    click_id(cx, s.win, ("start-action", 0usize));
    assert_asks(cx, &s);
    click_id(cx, s.win, "pick-cancel");
    assert!(!present(cx, s.win, "pick-connection-dialog"));
    assert!(tab_conns(cx, &s).is_empty());
    assert!(present(cx, s.win, "start-card"));
}

#[gpui_kit::test]
fn new_tab_in_a_workspace_without_connections_opens_the_new_connection_dialog(
    cx: &mut TestAppContext,
) {
    let s = started(cx, "dbento = 1\n");
    ctrl_t(cx, &s);
    wait_real(cx, "the dialog", |cx| present(cx, s.win, "conn-name"));
    assert!(!present(cx, s.win, "pick-connection-dialog"));
}
