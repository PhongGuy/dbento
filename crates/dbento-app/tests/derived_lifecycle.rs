//! Children across a rename, a restart and a reload.
use crate::derived_tabs::{PARENT, tab_conns, with_shell};
use crate::support::{
    click_id, init, init_tokio, label_of, present, reload, right_click, services, started,
    wait_real,
};
use dbento_app::app::{AppPaths, SavedState, TabState, WorkspaceSession, start};
use dbento_app::explorer::connection_id;
use dbento_app::persist::save_json;
use dbento_app::query_tab::ConnState;
use dbento_app::shell::runs_on;
use dbento_core::Workspace;
use gpui_kit::TestAppContext;

#[test]
fn a_tab_runs_on_its_connection_and_its_parent() {
    assert!(runs_on("srv", "srv"));
    assert!(runs_on("srv/app", "srv"));
    assert!(runs_on("srv/app", "srv/app"));
    assert!(!runs_on("srv2", "srv"));
    assert!(!runs_on("srv", "srv/app"));
}

#[gpui_kit::test]
fn a_parent_with_a_child_tab_cannot_be_renamed_or_deleted(cx: &mut TestAppContext) {
    let s = started(cx, PARENT);
    with_shell(cx, &s, |sh, w, cx| sh.open_sql("srv/app", "", w, cx));
    for item in ["menu-rename", "menu-delete"] {
        right_click(cx, &s, connection_id("srv"));
        click_id(cx, s.win, item);
        wait_real(cx, "the refusal", |cx| present(cx, s.win, "save-error-ok"));
        assert_eq!(
            label_of(cx, s.win, "confirm-detail").as_deref(),
            Some("Close its tabs first")
        );
        click_id(cx, s.win, "save-error-ok");
    }
    assert_eq!(s.toml(), PARENT);
}

#[gpui_kit::test]
fn a_child_tab_comes_back_after_a_restart(cx: &mut TestAppContext) {
    init(cx);
    init_tokio(cx);
    let (dir, files) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    std::fs::write(dir.path().join("dbento.toml"), PARENT).unwrap();
    let workspace = Workspace::open(dir.path()).unwrap().dir().to_path_buf();
    let tab = |c: &str, d: &str| TabState {
        connection: c.into(),
        draft: d.into(),
        ..Default::default()
    };
    let mut state = SavedState::default();
    state.put_session(WorkspaceSession {
        workspace,
        tabs: vec![tab("srv/app", "d0"), tab("srv/gone", "d1")],
        active: Some(0),
        ..Default::default()
    });
    save_json(&AppPaths::in_dir(files.path()).state, &state).unwrap();
    let (_win, shell) = cx
        .update(|cx| start(services(files.path()), Some(dir.path().into()), cx))
        .unwrap();
    wait_real(cx, "the restored tab", |cx| {
        shell.read_with(cx, |sh, _| !sh.tabs().is_empty())
    });
    let conns: Vec<String> = shell.read_with(cx, |sh, cx| {
        sh.tabs()
            .iter()
            .map(|t| t.read(cx).model().read(cx).spec().connection.clone())
            .collect()
    });
    assert_eq!(
        conns,
        ["srv/app"],
        "a listed child returns; an unlisted one does not"
    );
}

#[gpui_kit::test]
fn a_child_tab_goes_gone_when_its_parent_drops_databases(cx: &mut TestAppContext) {
    let s = started(cx, PARENT);
    with_shell(cx, &s, |sh, w, cx| sh.open_sql("srv/app", "", w, cx));
    assert_eq!(tab_conns(cx, &s), ["srv/app"]);
    let plain = PARENT.replace("databases = [\"app\", \"booking\"]\n", "");
    std::fs::write(s.dir.path().join("dbento.toml"), plain).unwrap();
    reload(cx, s.win);
    wait_real(cx, "the tab gone", |cx| {
        s.shell
            .read_with(cx, |sh, cx| sh.tab().unwrap().read(cx).conn_state())
            == ConnState::Gone
    });
}
