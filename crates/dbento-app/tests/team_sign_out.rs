//! Sign out revokes and forgets the token but keeps the binding; the signed-out chip signs
//! the same account in again and the sync resumes.
use dbento_app::team_dialog::{TeamDialogMode, TeamField};
use dbento_app::team_view::{NOT_REVOKED, chip_tooltip};
use dbento_core::{SyncState, TeamFile, TokenStore as _};
use dbento_testkit::potluck::Reply;
use gpui_kit::{AppContext as _, TestAppContext};

use crate::support::team::{TeamStarted, start_on, team_fx, team_fx_with, team_started, wait_view};
use crate::support::{click_id, present, wait_real};

fn sign_out(cx: &mut TestAppContext, s: &TeamStarted) {
    let shell = s.shell.clone();
    cx.update_window(s.win, |_, w, cx| {
        shell.update(cx, |sh, cx| sh.team_sign_out(w, cx))
    })
    .unwrap();
}
fn note(cx: &mut TestAppContext, s: &TeamStarted) -> Option<String> {
    s.shell.read_with(cx, |sh, cx| {
        sh.team()
            .and_then(|t| t.read(cx).revoke_note().map(str::to_owned))
    })
}

#[gpui_kit::test]
fn sign_out_revokes_forgets_and_shows_signed_out(cx: &mut TestAppContext) {
    let fx = team_fx("write");
    let s = team_started(cx, &fx);
    wait_view(cx, &s, "synced", |v| v.state == SyncState::Synced);
    sign_out(cx, &s);
    wait_view(cx, &s, "signed out", |v| v.state == SyncState::SignedOut);
    assert_eq!(fx.stub.hits("DELETE", "/api/v1/tokens/42"), 1);
    assert!(fx.tokens.get(&fx.account()).unwrap().is_none());
    let data = TeamFile::new(&fx.paths.team).load().unwrap();
    assert!(data.account(&fx.server(), Some("ada")).is_none());
    assert!(data.binding_for(&fx.ws).is_some());
    assert_eq!(note(cx, &s), None, "a revoked token needs no note");
}

#[gpui_kit::test]
fn a_token_the_server_did_not_revoke_is_named_while_signed_out(cx: &mut TestAppContext) {
    let fx = team_fx("write");
    let s = team_started(cx, &fx);
    wait_view(cx, &s, "synced", |v| v.state == SyncState::Synced);
    fx.stub
        .reply("DELETE", "/api/v1/tokens/42", Reply::Status(500));
    sign_out(cx, &s);
    wait_view(cx, &s, "signed out", |v| v.state == SyncState::SignedOut);
    assert_eq!(fx.stub.hits("DELETE", "/api/v1/tokens/42"), 1);
    assert!(fx.tokens.get(&fx.account()).unwrap().is_none());
    assert_eq!(note(cx, &s).as_deref(), Some(NOT_REVOKED));
    let view = s.shell.read_with(cx, |sh, cx| {
        sh.team().and_then(|t| t.read(cx).view().cloned())
    });
    assert_eq!(
        chip_tooltip(view.as_ref(), Some(NOT_REVOKED)).as_deref(),
        Some(NOT_REVOKED),
        "the signed-out chip's tooltip says so"
    );
    assert_eq!(chip_tooltip(view.as_ref(), None), None);
}

#[gpui_kit::test]
fn the_signed_out_chip_signs_the_same_account_in_and_sync_resumes(cx: &mut TestAppContext) {
    let fx = team_fx("write");
    let s = team_started(cx, &fx);
    wait_view(cx, &s, "synced", |v| v.state == SyncState::Synced);
    // An unrevoked token leaves a note, so the sign-in below has one to clear.
    fx.stub
        .reply("DELETE", "/api/v1/tokens/42", Reply::Status(500));
    sign_out(cx, &s);
    wait_view(cx, &s, "signed out", |v| v.state == SyncState::SignedOut);
    assert!(note(cx, &s).is_some(), "the note is up before signing in");
    click_id(cx, s.win, "team-status");
    wait_real(cx, "the dialog", |cx| present(cx, s.win, "team-password"));
    let dialog = s
        .shell
        .read_with(cx, |sh, _| sh.team_dialog().cloned())
        .unwrap();
    let mode = dialog.read_with(cx, |d, _| d.mode().clone());
    assert_eq!(
        mode,
        TeamDialogMode::SignIn {
            server: fx.server().to_string(),
            username: "ada".into()
        }
    );
    let server = dialog.read_with(cx, |d, cx| {
        d.input(TeamField::Server).read(cx).value().to_string()
    });
    assert_eq!(server, fx.server().to_string());
    let password = dialog.read_with(cx, |d, _| d.input(TeamField::Password).clone());
    cx.update_window(s.win, |_, w, cx| {
        password.update(cx, |i, cx| i.set_value("pw".to_owned(), w, cx))
    })
    .unwrap();
    click_id(cx, s.win, "team-sign-in");
    wait_view(cx, &s, "synced again", |v| v.state == SyncState::Synced);
    assert!(s.shell.read_with(cx, |sh, _| sh.team_dialog().is_none()));
    assert!(fx.tokens.get(&fx.account()).unwrap().is_some());
    assert_eq!(note(cx, &s), None, "signing in again clears any note");
}

#[gpui_kit::test]
fn sign_out_outside_a_team_workspace_changes_nothing(cx: &mut TestAppContext) {
    let fx = team_fx_with("write", false);
    let s = start_on(cx, &fx.ws, Some(fx.hooks()));
    sign_out(cx, &s);
    cx.run_until_parked();
    assert_eq!(fx.stub.hits("DELETE", "/api/v1/tokens/42"), 0);
    assert!(s.shell.read_with(cx, |sh, _| sh.team().is_none()));
}
