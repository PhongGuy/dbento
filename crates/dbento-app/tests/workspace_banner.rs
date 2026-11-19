//! A bad dbento.toml keeps the last good workspace behind a banner; a failed watcher says so
//! and offers Reload.
use crate::support::{
    Opened, click_id, files_changed, label_of, opened, present, reload, rewrite, settle_reload,
};
use dbento_app::query_tab::ConnState;
use dbento_core::WatchEvent;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, Role, TestAppContext};

const AAA: &str = "[connections.aaa]\nengine = \"sqlite\"\npath = \"data/app.db\"\n";
const BROKEN: &str = "dbento = 1\n[connections.lite]\nengine = \"nope\"\n";

fn toml(o: &Opened) -> std::path::PathBuf {
    o.fx.dir.path().join("dbento.toml")
}
fn declared(cx: &mut TestAppContext, o: &Opened, name: &str) -> bool {
    o.shell
        .read_with(cx, |s, _| s.workspace().unwrap().declared(name).is_some())
}
fn state(cx: &mut TestAppContext, o: &Opened) -> ConnState {
    o.shell
        .read_with(cx, |s, cx| s.tab().unwrap().read(cx).conn_state())
}
fn role(cx: &mut TestAppContext, o: &Opened, id: &'static str) -> Option<Role> {
    cx.update_window(o.win, |_, w, cx| {
        w.render_frame(cx);
        w.try_find(id).and_then(|e| e.role())
    })
    .unwrap()
}

#[test]
fn the_banner_texts() {
    use dbento_app::workspace_reload::{toml_problem_text, watch_problem_text};
    assert_eq!(
        toml_problem_text("bad key."),
        "dbento.toml has an error: bad key. Showing the last version that loaded."
    );
    assert_eq!(
        watch_problem_text("limit"),
        "Not watching this folder for changes: limit. Use Reload after changing files."
    );
}

#[gpui_kit::test]
fn a_broken_file_keeps_the_last_good_workspace_behind_the_banner(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    std::fs::write(toml(&o), BROKEN).unwrap();
    files_changed(cx, &o);
    assert!(declared(cx, &o, "nopg"), "the last good workspace stays");
    let text = label_of(cx, o.win, "workspace-banner-error").unwrap();
    assert!(
        text.starts_with("dbento.toml has an error: ") && text.contains("lite"),
        "{text}"
    );
    assert_eq!(
        state(cx, &o),
        ConnState::Current,
        "a bad read never turns a tab Gone"
    );
    assert_eq!(role(cx, &o, "workspace-banner"), Some(Role::Status));
    assert_eq!(role(cx, &o, "workspace-banner-error"), Some(Role::Label));
    rewrite(&o.fx, "", AAA);
    files_changed(cx, &o);
    assert!(declared(cx, &o, "aaa"), "the next good read installs");
    assert!(
        !present(cx, o.win, "workspace-banner"),
        "and clears the banner"
    );
}

#[gpui_kit::test]
fn a_deleted_file_keeps_the_workspace_and_says_so(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    std::fs::remove_file(toml(&o)).unwrap();
    files_changed(cx, &o);
    assert!(declared(cx, &o, "lite"), "kept");
    assert_eq!(state(cx, &o), ConnState::Current);
    assert!(
        present(cx, o.win, "workspace-banner-error"),
        "the banner shows"
    );
}

#[gpui_kit::test]
fn a_file_fixed_before_the_retry_installs_without_a_banner(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    std::fs::write(toml(&o), BROKEN).unwrap();
    let shell = o.shell.clone();
    cx.update_window(o.win, |_, w, cx| {
        shell.update(cx, |s, cx| s.workspace_files_changed(w, cx))
    })
    .unwrap();
    cx.run_until_parked();
    assert!(
        o.shell.read_with(cx, |s, _| s.reloading()),
        "waiting to read again"
    );
    rewrite(&o.fx, "", AAA);
    settle_reload(cx, &o.shell);
    assert!(declared(cx, &o, "aaa"), "the retry read the fixed file");
    assert!(
        !present(cx, o.win, "workspace-banner"),
        "no banner for a mid-write read"
    );
}

#[gpui_kit::test]
fn the_reload_menu_reports_a_broken_file_in_the_banner(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    std::fs::write(toml(&o), BROKEN).unwrap();
    reload(cx, o.win);
    settle_reload(cx, &o.shell);
    assert!(
        present(cx, o.win, "workspace-banner-error"),
        "the banner, not a dialog"
    );
    assert!(!present(cx, o.win, "confirm-detail"), "no error dialog");
}

#[gpui_kit::test]
fn a_failed_watcher_offers_reload(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let shell = o.shell.clone();
    let generation = shell.read_with(cx, |s, _| s.watch_generation());
    let failed = WatchEvent::Failed {
        reason: "watch limit reached".into(),
    };
    cx.update_window(o.win, |_, w, cx| {
        shell.update(cx, |s, cx| s.watch_event(generation, failed, w, cx))
    })
    .unwrap();
    assert_eq!(
        label_of(cx, o.win, "workspace-banner-watch").as_deref(),
        Some(
            "Not watching this folder for changes: watch limit reached. Use Reload after changing files."
        )
    );
    rewrite(&o.fx, "", AAA);
    click_id(cx, o.win, "workspace-banner-reload");
    settle_reload(cx, &o.shell);
    assert!(declared(cx, &o, "aaa"), "Reload reread the file");
    assert!(
        present(cx, o.win, "workspace-banner-watch"),
        "still not watching"
    );
}
