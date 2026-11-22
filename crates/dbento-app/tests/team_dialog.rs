//! The sign-in and join dialog: credentials, the workspace list, the clone into a folder and
//! the switch to it; a wrong password and a remote plain-http server each stop it.
use dbento_app::explorer::part_id;
use dbento_app::team_dialog::{TeamDialogMode, TeamField, TeamStep};
use dbento_core::{SyncState, TeamFile, TokenStore as _};
use dbento_testkit::potluck::Reply;
use gpui_kit::{AppContext as _, TestAppContext};

use crate::support::team::{TeamFx, TeamStarted, start_on, team_fx_with, wait_view};
use crate::support::{click_id, label_of, present, wait_real};

fn open(cx: &mut TestAppContext, s: &TeamStarted) {
    let shell = s.shell.clone();
    cx.update_window(s.win, |_, w, cx| {
        shell.update(cx, |sh, cx| {
            sh.open_team_dialog(TeamDialogMode::Join, w, cx)
        })
    })
    .unwrap();
    wait_real(cx, "the dialog", |cx| present(cx, s.win, "team-server"));
}
fn fill(cx: &mut TestAppContext, s: &TeamStarted, field: TeamField, text: &str) {
    let dialog = s
        .shell
        .read_with(cx, |sh, _| sh.team_dialog().cloned())
        .unwrap();
    let input = dialog.read_with(cx, |d, _| d.input(field).clone());
    cx.update_window(s.win, |_, w, cx| {
        input.update(cx, |i, cx| i.set_value(text.to_owned(), w, cx))
    })
    .unwrap();
    cx.run_until_parked();
}
fn signed_in_list(cx: &mut TestAppContext, fx: &TeamFx, s: &TeamStarted) {
    open(cx, s);
    fill(cx, s, TeamField::Server, fx.stub.uri());
    fill(cx, s, TeamField::User, "ada");
    fill(cx, s, TeamField::Password, "pw");
    click_id(cx, s.win, "team-sign-in");
    wait_real(cx, "the workspace list", |cx| {
        present(cx, s.win, part_id("team-workspace", "acme/shared"))
    });
}

#[gpui_kit::test]
fn sign_in_lists_the_workspaces_and_join_clones_and_opens_the_folder(cx: &mut TestAppContext) {
    let fx = team_fx_with("write", false);
    let s = start_on(cx, &fx.ws, Some(fx.hooks()));
    signed_in_list(cx, &fx, &s);
    assert_eq!(fx.stub.hits("POST", "/api/v1/session"), 1);
    assert!(fx.tokens.get(&fx.account()).unwrap().is_some());
    let target = fx.root.join("joined");
    click_id(cx, s.win, part_id("team-workspace", "acme/shared"));
    fill(cx, &s, TeamField::Folder, target.to_str().unwrap());
    click_id(cx, s.win, "team-join");
    let real = || std::fs::canonicalize(&target).ok();
    wait_real(cx, "the switch", |cx| {
        let open = s
            .shell
            .read_with(cx, |sh, _| sh.workspace().map(|w| w.dir().to_path_buf()));
        open.is_some() && open == real()
    });
    assert!(s.shell.read_with(cx, |sh, _| sh.team_dialog().is_none()));
    wait_view(cx, &s, "synced", |v| v.state == SyncState::Synced);
    assert_eq!(
        std::fs::read_to_string(target.join("queries/a.sql")).unwrap(),
        "select 1;\n"
    );
    let data = TeamFile::new(&fx.paths.team).load().unwrap();
    assert!(data.binding_for(&target).is_some());
}

#[gpui_kit::test]
fn a_wrong_password_says_so_and_keeps_the_first_step(cx: &mut TestAppContext) {
    let fx = team_fx_with("write", false);
    fx.stub.reply("POST", "/api/v1/session", Reply::Status(401));
    let s = start_on(cx, &fx.ws, Some(fx.hooks()));
    open(cx, &s);
    fill(cx, &s, TeamField::Server, fx.stub.uri());
    fill(cx, &s, TeamField::User, "ada");
    fill(cx, &s, TeamField::Password, "nope");
    click_id(cx, s.win, "team-sign-in");
    wait_real(cx, "the error", |cx| present(cx, s.win, "team-error"));
    assert_eq!(
        label_of(cx, s.win, "team-error").as_deref(),
        Some("The server did not accept that user name and password.")
    );
    let dialog = s
        .shell
        .read_with(cx, |sh, _| sh.team_dialog().cloned())
        .unwrap();
    assert_eq!(dialog.read_with(cx, |d, _| d.step()), TeamStep::Credentials);
    assert!(fx.tokens.get(&fx.account()).unwrap().is_none());
}

#[gpui_kit::test]
fn remote_plain_http_holds_sign_in_until_ticked(cx: &mut TestAppContext) {
    let fx = team_fx_with("write", false);
    let s = start_on(cx, &fx.ws, Some(fx.hooks()));
    open(cx, &s);
    fill(cx, &s, TeamField::Server, "http://team.example");
    fill(cx, &s, TeamField::User, "ada");
    fill(cx, &s, TeamField::Password, "pw");
    assert!(present(cx, s.win, "team-insecure"));
    let dialog = s
        .shell
        .read_with(cx, |sh, _| sh.team_dialog().cloned())
        .unwrap();
    assert!(!dialog.read_with(cx, |d, cx| d.can_sign_in(cx)));
    // A held button starts nothing: no sign-in in flight and no error.
    click_id(cx, s.win, "team-sign-in");
    assert!(!dialog.read_with(cx, |d, _| d.signing_in()));
    assert!(!present(cx, s.win, "team-error"));
    // Ticked, it may go; it is not clicked again, so nothing leaves the machine.
    click_id(cx, s.win, "team-insecure");
    assert!(dialog.read_with(cx, |d, cx| d.can_sign_in(cx)));
    fill(cx, &s, TeamField::Server, fx.stub.uri());
    assert!(!present(cx, s.win, "team-insecure"));
}
