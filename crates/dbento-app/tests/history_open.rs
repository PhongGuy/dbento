//! Opening a history row: on its own connection, or, when that name no longer resolves, on
//! none until the user picks one.
use crate::derived_tabs::{tab_conns, with_shell};
use crate::support::{Started, click_id, label_of, present, started};
use gpui_kit::TestAppContext;

const TWO: &str = "dbento = 1\n[connections.lite]\nengine = \"sqlite\"\npath = \"app.db\"\nread_only = true\n[connections.rw]\nengine = \"sqlite\"\npath = \"app.db\"\n[connections.srv]\nengine = \"postgres\"\nhost = \"127.0.0.1\"\nport = 9\ndatabase = \"x\"\nuser = \"x\"\ndatabases = [\"app\"]\n";

fn tab_sql(cx: &mut TestAppContext, s: &Started, ix: usize) -> String {
    s.shell
        .read_with(cx, |sh, cx| sh.tabs()[ix].read(cx).sql(cx))
}

// A row whose connection is gone opens no tab on the active one; it names the missing
// connection and asks which to use.
fn assert_asks_instead(cx: &mut TestAppContext, s: &Started, gone: &str) {
    with_shell(cx, s, |sh, w, cx| sh.open_connection_named("rw", w, cx));
    with_shell(cx, s, |sh, w, cx| sh.open_history(gone, "SELECT 1", w, cx));
    assert_eq!(
        tab_conns(cx, s),
        ["rw"],
        "nothing opens on the active tab's"
    );
    assert_eq!(
        label_of(cx, s.win, "notice").as_deref(),
        Some(format!("{gone} no longer exists; pick a connection to run this.").as_str()),
        "the notice names the missing connection"
    );
    assert!(
        present(cx, s.win, "pick-connection-dialog"),
        "the picker asks which connection to use"
    );
}

#[gpui_kit::test]
fn a_removed_connection_opens_nothing_until_one_is_picked(cx: &mut TestAppContext) {
    let s = started(cx, TWO);
    assert_asks_instead(cx, &s, "pg-old");
    assert_eq!(
        label_of(cx, s.win, ("pick-connection", 0usize)).as_deref(),
        Some("lite")
    );
    click_id(cx, s.win, ("pick-connection", 0usize));
    assert!(!present(cx, s.win, "pick-connection-dialog"));
    assert_eq!(tab_conns(cx, &s), ["rw", "lite"]);
    assert_eq!(tab_sql(cx, &s, 1), "SELECT 1");
}

#[gpui_kit::test]
fn a_renamed_connection_asks_the_same_and_cancel_opens_nothing(cx: &mut TestAppContext) {
    // The row still names `lite-old`, which the workspace now calls `lite`.
    let s = started(cx, TWO);
    assert_asks_instead(cx, &s, "lite-old");
    click_id(cx, s.win, "pick-cancel");
    assert!(!present(cx, s.win, "pick-connection-dialog"));
    assert_eq!(tab_conns(cx, &s), ["rw"]);
}

#[gpui_kit::test]
fn a_row_on_a_present_connection_opens_on_it(cx: &mut TestAppContext) {
    let s = started(cx, TWO);
    with_shell(cx, &s, |sh, w, cx| sh.open_connection_named("rw", w, cx));
    with_shell(cx, &s, |sh, w, cx| {
        sh.open_history("lite", "SELECT 2", w, cx)
    });
    with_shell(cx, &s, |sh, w, cx| {
        sh.open_history("srv/app", "SELECT 3", w, cx)
    });
    assert_eq!(tab_conns(cx, &s), ["rw", "lite", "srv/app"]);
    assert_eq!(tab_sql(cx, &s, 1), "SELECT 2");
    assert!(!present(cx, s.win, "pick-connection-dialog"));
    assert!(!present(cx, s.win, "notice"));
}
