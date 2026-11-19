//! The reload path outside changes take: dbento.toml reread, compared and installed, own
//! writes absorbed, bursts coalesced, old generations and closing windows ignored.
use crate::support::{Opened, files_changed, label_of, opened, rewrite, settle_reload};
use dbento_app::explorer::connection_id;
use dbento_app::query_tab::ConnState;
use dbento_app::workspace_reload::{DiskRead, TomlVerdict, judge_toml};
use dbento_core::{Edit, FolderPath, WatchEvent, Workspace};
use gpui_kit::{AppContext as _, TestAppContext};
use std::sync::Arc;

const AAA: &str = "[connections.aaa]\nengine = \"sqlite\"\npath = \"data/app.db\"\n";

fn installed(cx: &mut TestAppContext, o: &Opened) -> Arc<Workspace> {
    o.shell
        .read_with(cx, |s, _| Arc::clone(s.workspace().unwrap()))
}

#[test]
fn judge_toml_drops_stale_reads_and_skips_equal_text() {
    let text = |t: &str| DiskRead::Text(t.to_owned());
    assert_eq!(
        judge_toml("a", "b", &text("c")),
        TomlVerdict::Stale,
        "the app wrote meanwhile"
    );
    assert_eq!(
        judge_toml("a", "b", &DiskRead::Missing),
        TomlVerdict::Stale,
        "stale wins"
    );
    assert_eq!(judge_toml("a", "a", &text("a")), TomlVerdict::Same);
    assert_eq!(
        judge_toml("a", "a", &text("b")),
        TomlVerdict::Parse("b".into())
    );
    assert_eq!(
        judge_toml("a", "a", &DiskRead::Missing),
        TomlVerdict::Missing
    );
    let denied = DiskRead::Unreadable("denied".into());
    assert_eq!(
        judge_toml("a", "a", &denied),
        TomlVerdict::Unreadable("denied".into())
    );
}

#[gpui_kit::test]
fn a_connection_added_outside_shows_in_the_explorer(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    rewrite(&o.fx, "", AAA);
    files_changed(cx, &o);
    assert!(installed(cx, &o).declared("aaa").is_some(), "installed");
    assert_eq!(
        label_of(cx, o.win, connection_id("aaa")).as_deref(),
        Some("aaa")
    );
}

#[gpui_kit::test]
fn a_session_change_reaches_the_open_tab(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    rewrite(&o.fx, "statement_timeout = \"5s\"", "");
    files_changed(cx, &o);
    let state = o
        .shell
        .read_with(cx, |s, cx| s.tab().unwrap().read(cx).conn_state());
    assert_eq!(
        state,
        ConnState::Changed {
            still_read_write: false
        }
    );
}

#[gpui_kit::test]
fn an_unchanged_file_installs_nothing(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let before = installed(cx, &o);
    files_changed(cx, &o);
    assert!(
        Arc::ptr_eq(&before, &installed(cx, &o)),
        "equal text is no change"
    );
}

#[gpui_kit::test]
fn the_apps_own_write_reads_back_as_no_change(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let before = installed(cx, &o);
    let shell = o.shell.clone();
    let path: FolderPath = "team".parse().unwrap();
    let edit = Edit::PutFolder { path, color: None };
    cx.update_window(o.win, |_, w, cx| {
        shell.update(cx, |s, cx| assert!(s.apply_edit(edit, w, cx), "written"))
    })
    .unwrap();
    let after_edit = installed(cx, &o);
    assert_ne!(
        before.source_text(),
        after_edit.source_text(),
        "the edit wrote"
    );
    files_changed(cx, &o);
    assert!(
        Arc::ptr_eq(&after_edit, &installed(cx, &o)),
        "the echo installs nothing"
    );
}

#[gpui_kit::test]
fn a_burst_runs_one_reload_and_one_follow_up(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let before = o.shell.read_with(cx, |s, _| s.reloads_run());
    let shell = o.shell.clone();
    cx.update_window(o.win, |_, w, cx| {
        shell.update(cx, |s, cx| {
            for _ in 0..5 {
                s.workspace_files_changed(w, cx);
            }
        })
    })
    .unwrap();
    settle_reload(cx, &o.shell);
    assert_eq!(o.shell.read_with(cx, |s, _| s.reloads_run()) - before, 2);
}

#[gpui_kit::test]
fn an_old_generations_event_is_dropped(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    rewrite(&o.fx, "", AAA);
    let shell = o.shell.clone();
    let current = shell.read_with(cx, |s, _| s.watch_generation());
    let send = |cx: &mut TestAppContext, generation: u64| {
        let shell = shell.clone();
        cx.update_window(o.win, |_, w, cx| {
            shell.update(cx, |s, cx| {
                s.watch_event(generation, WatchEvent::Changed, w, cx)
            })
        })
        .unwrap();
        settle_reload(cx, &shell);
    };
    send(cx, current.wrapping_sub(1));
    assert!(
        installed(cx, &o).declared("aaa").is_none(),
        "an older watcher's event"
    );
    send(cx, current);
    assert!(
        installed(cx, &o).declared("aaa").is_some(),
        "the current watcher's event"
    );
}

#[gpui_kit::test]
fn nothing_reloads_while_the_window_closes(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    rewrite(&o.fx, "", AAA);
    let shell = o.shell.clone();
    let started = cx
        .update_window(o.win, |_, w, cx| {
            shell.update(cx, |s, cx| {
                s.request_close(w, cx);
                s.workspace_files_changed(w, cx);
                s.reloading()
            })
        })
        .unwrap();
    assert!(!started, "a closing window starts no reload");
}
