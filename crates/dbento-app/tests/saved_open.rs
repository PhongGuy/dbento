//! Opening a saved query: on its folder's connection, or, when that folder names a
//! connection that no longer resolves, on none until the user picks one.
use crate::derived_tabs::{tab_conns, with_shell};
use crate::support::{Started, click_id, label_of, present, started};
use dbento_core::SavedQuery;
use gpui_kit::TestAppContext;
use std::path::Path;

const TWO: &str = "dbento = 1\n[connections.lite]\nengine = \"sqlite\"\npath = \"app.db\"\nread_only = true\n[connections.rw]\nengine = \"sqlite\"\npath = \"app.db\"\n[connections.srv]\nengine = \"postgres\"\nhost = \"127.0.0.1\"\nport = 9\ndatabase = \"x\"\nuser = \"x\"\ndatabases = [\"app\"]\n";

// Writes `rel` under the workspace and returns it as the workspace lists it.
fn saved(cx: &mut TestAppContext, s: &Started, rel: &str, text: &str) -> SavedQuery {
    let path = s.dir.path().join(rel);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, text).unwrap();
    let ws = s
        .shell
        .read_with(cx, |sh, _| sh.workspace().cloned().unwrap());
    ws.saved_queries()
        .unwrap()
        .into_iter()
        .find(|q| q.path() == Path::new(rel))
        .unwrap()
}

fn tab_text_and_file(cx: &mut TestAppContext, s: &Started, ix: usize) -> (String, Option<String>) {
    s.shell.read_with(cx, |sh, cx| {
        let t = sh.tabs()[ix].read(cx);
        (t.sql(cx), t.file().map(|p| p.display().to_string()))
    })
}

// A file whose folder names a gone connection opens no tab on the active one; it names the
// missing connection and asks which to use.
fn assert_asks_instead(cx: &mut TestAppContext, s: &Started, q: &SavedQuery, gone: &str) {
    with_shell(cx, s, |sh, w, cx| sh.open_connection_named("rw", w, cx));
    with_shell(cx, s, |sh, w, cx| sh.open_saved(q, w, cx));
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
fn a_removed_connections_query_opens_bound_on_the_picked_one(cx: &mut TestAppContext) {
    let s = started(cx, TWO);
    let q = saved(cx, &s, "queries/pg-old/q.sql", "SELECT 1");
    assert_asks_instead(cx, &s, &q, "pg-old");
    assert_eq!(
        label_of(cx, s.win, ("pick-connection", 0usize)).as_deref(),
        Some("lite")
    );
    click_id(cx, s.win, ("pick-connection", 0usize));
    assert!(!present(cx, s.win, "pick-connection-dialog"));
    assert_eq!(tab_conns(cx, &s), ["rw", "lite"]);
    assert_eq!(
        tab_text_and_file(cx, &s, 1),
        (
            "SELECT 1".to_owned(),
            Some("queries/pg-old/q.sql".to_owned())
        ),
        "the tab stays bound to its file"
    );
}

#[gpui_kit::test]
fn a_renamed_connections_query_asks_the_same_and_cancel_opens_nothing(cx: &mut TestAppContext) {
    // The folder still says `lite-old`, which the workspace now calls `lite`.
    let s = started(cx, TWO);
    let q = saved(cx, &s, "queries/lite-old/q.sql", "SELECT 1");
    assert_asks_instead(cx, &s, &q, "lite-old");
    click_id(cx, s.win, "pick-cancel");
    assert!(!present(cx, s.win, "pick-connection-dialog"));
    assert_eq!(tab_conns(cx, &s), ["rw"]);
}

#[gpui_kit::test]
fn a_gone_child_databases_query_names_the_child(cx: &mut TestAppContext) {
    let s = started(cx, TWO);
    let q = saved(cx, &s, "queries/srv/booking/q.sql", "SELECT 1");
    assert_asks_instead(cx, &s, &q, "srv/booking");
}

#[gpui_kit::test]
fn a_query_on_a_present_or_child_connection_opens_on_it(cx: &mut TestAppContext) {
    let s = started(cx, TWO);
    let lite = saved(cx, &s, "queries/lite/a.sql", "SELECT 2");
    let child = saved(cx, &s, "queries/srv/app/b.sql", "SELECT 3");
    with_shell(cx, &s, |sh, w, cx| sh.open_connection_named("rw", w, cx));
    with_shell(cx, &s, |sh, w, cx| sh.open_saved(&lite, w, cx));
    with_shell(cx, &s, |sh, w, cx| sh.open_saved(&child, w, cx));
    assert_eq!(tab_conns(cx, &s), ["rw", "lite", "srv/app"]);
    assert_eq!(
        tab_text_and_file(cx, &s, 1),
        ("SELECT 2".to_owned(), Some("queries/lite/a.sql".to_owned()))
    );
    assert!(!present(cx, s.win, "pick-connection-dialog"));
    assert!(!present(cx, s.win, "notice"));
}

#[gpui_kit::test]
fn a_query_outside_any_folder_still_opens_on_the_active_connection(cx: &mut TestAppContext) {
    let s = started(cx, TWO);
    let q = saved(cx, &s, "queries/loose.sql", "SELECT 4");
    with_shell(cx, &s, |sh, w, cx| sh.open_connection_named("rw", w, cx));
    with_shell(cx, &s, |sh, w, cx| sh.open_saved(&q, w, cx));
    assert_eq!(tab_conns(cx, &s), ["rw", "rw"]);
    assert!(!present(cx, s.win, "pick-connection-dialog"));
}
