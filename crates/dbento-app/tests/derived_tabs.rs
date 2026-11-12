//! Tabs on derived child connections: every way of opening one keeps the child's name.
use crate::support::{Started, started};
use dbento_app::shell::Shell;
use dbento_app::sidebar::SidebarEvent;
use dbento_core::{RelationKind, RelationRef};
use gpui_kit::{AppContext as _, Context, TestAppContext, Window};
use std::path::Path;

pub(crate) const PARENT: &str = "dbento = 1\n[connections.lite]\nengine = \"sqlite\"\npath = \"app.db\"\n[connections.srv]\nengine = \"postgres\"\nhost = \"127.0.0.1\"\nport = 9\ndatabase = \"x\"\nuser = \"x\"\ndatabases = [\"app\", \"booking\"]\n";

pub(crate) fn with_shell<R>(
    cx: &mut TestAppContext,
    s: &Started,
    f: impl FnOnce(&mut Shell, &mut Window, &mut Context<Shell>) -> R,
) -> R {
    let shell = s.shell.clone();
    let r = cx
        .update_window(s.win, |_, w, cx| shell.update(cx, |sh, cx| f(sh, w, cx)))
        .unwrap();
    cx.run_until_parked();
    r
}
pub(crate) fn tab_conns(cx: &mut TestAppContext, s: &Started) -> Vec<String> {
    s.shell.read_with(cx, |sh, cx| {
        sh.tabs()
            .iter()
            .map(|t| t.read(cx).model().read(cx).spec().connection.clone())
            .collect()
    })
}

#[gpui_kit::test]
fn a_query_on_a_child_opens_on_the_child(cx: &mut TestAppContext) {
    let s = started(cx, PARENT);
    with_shell(cx, &s, |sh, w, cx| {
        sh.open_sql("srv/app", "SELECT 1", w, cx)
    });
    assert_eq!(tab_conns(cx, &s), ["srv/app"]);
    with_shell(cx, &s, |sh, w, cx| {
        sh.open_sql("srv/nope", "SELECT 1", w, cx)
    });
    assert_eq!(
        tab_conns(cx, &s),
        ["srv/app", "srv/app"],
        "an unresolved name falls back to the active tab's"
    );
}

#[gpui_kit::test]
fn duplicate_and_new_tab_stay_on_the_child(cx: &mut TestAppContext) {
    let s = started(cx, PARENT);
    with_shell(cx, &s, |sh, w, cx| {
        sh.open_connection_named("srv/booking", w, cx)
    });
    with_shell(cx, &s, |sh, w, cx| sh.duplicate_tab(0, w, cx));
    with_shell(cx, &s, |sh, w, cx| sh.new_tab(w, cx));
    assert_eq!(tab_conns(cx, &s), ["srv/booking"; 3]);
}

#[gpui_kit::test]
fn a_relation_and_a_filtered_preview_open_on_the_child(cx: &mut TestAppContext) {
    let s = started(cx, PARENT);
    let rel = RelationRef::new("public", "t");
    with_shell(cx, &s, |sh, w, cx| {
        sh.open_relation_named("srv/app", &rel, RelationKind::View, w, cx)
    });
    let opened = with_shell(cx, &s, |sh, w, cx| {
        sh.open_filtered("srv/app", &rel, "x = 1".into(), w, cx)
    });
    assert!(opened);
    let conns = tab_conns(cx, &s);
    assert!(
        !conns.is_empty() && conns.iter().all(|c| c == "srv/app"),
        "{conns:?}"
    );
}

#[gpui_kit::test]
fn the_explorers_open_relation_event_names_the_child(cx: &mut TestAppContext) {
    let s = started(cx, PARENT);
    let sidebar = s.sidebar(cx);
    sidebar.update(cx, |_, cx| {
        cx.emit(SidebarEvent::OpenRelation {
            connection: "srv/app".into(),
            rel: RelationRef::new("public", "t"),
            kind: RelationKind::View,
        })
    });
    cx.run_until_parked();
    assert_eq!(tab_conns(cx, &s), ["srv/app"]);
}

#[gpui_kit::test]
fn a_saved_query_in_a_child_folder_opens_on_the_child(cx: &mut TestAppContext) {
    let s = started(cx, PARENT);
    let dir = s.dir.path();
    std::fs::create_dir_all(dir.join("queries/srv/app")).unwrap();
    std::fs::write(dir.join("queries/srv/app/q.sql"), "SELECT 3").unwrap();
    let ws = s
        .shell
        .read_with(cx, |sh, _| sh.workspace().cloned().unwrap());
    let q = ws
        .saved_queries()
        .unwrap()
        .into_iter()
        .find(|q| q.path() == Path::new("queries/srv/app/q.sql"))
        .unwrap();
    assert_eq!(q.connection(), Some("srv/app"));
    with_shell(cx, &s, |sh, w, cx| sh.open_saved(&q, w, cx));
    assert_eq!(tab_conns(cx, &s), ["srv/app"]);
}
