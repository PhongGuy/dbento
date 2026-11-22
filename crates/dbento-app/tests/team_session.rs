//! The team session behind a bound workspace: started on open, fed by file changes, stopped
//! off the UI thread.
use std::time::{Duration, Instant};

use dbento_core::SyncState;
use gpui_kit::{AppContext as _, TestAppContext};

use crate::support::team::{
    TOML, start_on, team_fx, team_fx_with, team_started, touch, wait_view, write,
};
use crate::support::wait_real;

#[gpui_kit::test]
fn a_bound_folder_starts_its_sync_and_reports_synced(cx: &mut TestAppContext) {
    let fx = team_fx("write");
    let s = team_started(cx, &fx);
    let v = wait_view(cx, &s, "synced", |v| v.state == SyncState::Synced);
    assert_eq!(v.unpushed, 0);
    let dir = s
        .shell
        .read_with(cx, |sh, cx| sh.team().unwrap().read(cx).dir().to_path_buf());
    assert_eq!(dir, fx.ws);
}

#[gpui_kit::test]
fn a_changed_file_reaches_the_origin(cx: &mut TestAppContext) {
    let fx = team_fx("write");
    let s = team_started(cx, &fx);
    wait_view(cx, &s, "synced", |v| v.state == SyncState::Synced);
    write(&fx.ws, "queries/a.sql", "select 2;\n");
    touch(cx, &s);
    wait_real(cx, "the push", |_| {
        fx.origin_show("queries/a.sql").as_deref() == Some("select 2;\n")
    });
}

#[gpui_kit::test]
fn without_installed_hooks_a_bound_folder_has_no_session(cx: &mut TestAppContext) {
    let fx = team_fx("write");
    let s = start_on(cx, &fx.ws, None);
    assert!(s.shell.read_with(cx, |sh, _| sh.team().is_none()));
}

#[gpui_kit::test]
fn an_unbound_folder_has_no_session(cx: &mut TestAppContext) {
    let fx = team_fx_with("write", false);
    let s = start_on(cx, &fx.ws, Some(fx.hooks()));
    assert!(s.shell.read_with(cx, |sh, _| sh.team().is_none()));
    assert_eq!(
        std::fs::read_to_string(fx.ws.join("dbento.toml")).unwrap(),
        TOML
    );
}

#[gpui_kit::test]
fn closing_the_workspace_while_a_push_hangs_returns_at_once(cx: &mut TestAppContext) {
    let fx = team_fx("write");
    fx.hook("pre-receive", "sleep 30");
    let s = team_started(cx, &fx);
    wait_view(cx, &s, "synced", |v| v.state == SyncState::Synced);
    write(&fx.ws, "queries/a.sql", "select 2;\n");
    touch(cx, &s);
    wait_view(cx, &s, "the push", |v| v.state == SyncState::Pushing);
    let began = Instant::now();
    let shell = s.shell.clone();
    cx.update_window(s.win, |_, w, cx| {
        shell.update(cx, |sh, cx| sh.close_workspace(w, cx))
    })
    .unwrap();
    cx.run_until_parked();
    assert!(
        began.elapsed() < Duration::from_secs(2),
        "closing waited {:?}",
        began.elapsed()
    );
    assert!(s.shell.read_with(cx, |sh, _| sh.team().is_none()));
}

#[gpui_kit::test]
fn closing_the_window_waits_for_the_last_push(cx: &mut TestAppContext) {
    let fx = team_fx("write");
    fx.hook("pre-receive", "cat >/dev/null\nsleep 1");
    let s = team_started(cx, &fx);
    wait_view(cx, &s, "synced", |v| v.state == SyncState::Synced);
    write(&fx.ws, "queries/a.sql", "select 2;\n");
    touch(cx, &s);
    wait_view(cx, &s, "the push", |v| v.state == SyncState::Pushing);
    let shell = s.shell.clone();
    let closes = cx
        .update_window(s.win, |_, w, cx| {
            shell.update(cx, |sh, cx| sh.request_close(w, cx))
        })
        .unwrap();
    assert!(!closes, "the window closed with a push in flight");
    assert!(s.shell.read_with(cx, |sh, _| sh.pending_closings()) >= 1);
    wait_real(cx, "the push", |_| {
        fx.origin_show("queries/a.sql").as_deref() == Some("select 2;\n")
    });
}
