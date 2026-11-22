//! The status-bar team chip: its label per state, its retry, and its absence outside team mode.
use dbento_app::team::TeamView;
use dbento_app::team_view::{chip_detail, chip_label, retry_offered};
use dbento_core::SyncState;
use gpui_kit::TestAppContext;

use crate::support::team::{show, start_on, team_fx, team_fx_with, team_started, wait_view};
use crate::support::{click_id, label_of, present};

fn v(state: SyncState, unpushed: u32) -> TeamView {
    TeamView {
        state,
        unpushed,
        last_synced: None,
        note: None,
    }
}

#[test]
fn each_state_has_its_label() {
    assert_eq!(chip_label(None), "Connecting");
    let cases = [
        (v(SyncState::Synced, 0), "Synced"),
        (v(SyncState::Committing, 0), "Saving"),
        (v(SyncState::Pulling, 0), "Pulling"),
        (v(SyncState::Pushing, 0), "Pushing"),
        (v(SyncState::Offline, 0), "Offline"),
        (v(SyncState::Offline, 2), "Offline, 2 not sent"),
        (v(SyncState::ReadOnly, 0), "Read only"),
        (v(SyncState::SignedOut, 0), "Signed out"),
        (v(SyncState::SignedOut, 1), "Signed out, 1 not sent"),
        (v(SyncState::GitMissing, 0), "Git not found"),
        (v(SyncState::Busy, 0), "Busy"),
        (v(SyncState::Conflicted(vec![]), 0), "Conflicts in 0 files"),
        (v(SyncState::Failed("boom".into()), 0), "Sync failed"),
    ];
    for (view, want) in cases {
        assert_eq!(chip_label(Some(&view)), want, "{view:?}");
    }
    let mut ro = v(SyncState::ReadOnly, 0);
    ro.note = Some("local edits overlap the server's".into());
    assert_eq!(chip_label(Some(&ro)), "Read only, not following");
    assert_eq!(
        chip_detail(&ro).as_deref(),
        Some("local edits overlap the server's")
    );
    assert_eq!(
        chip_detail(&v(SyncState::Failed("boom".into()), 0)).as_deref(),
        Some("boom")
    );
    assert_eq!(chip_detail(&v(SyncState::Synced, 0)), None);
}

#[test]
fn retry_is_offered_only_where_a_retry_can_help() {
    for s in [
        SyncState::Offline,
        SyncState::Busy,
        SyncState::GitMissing,
        SyncState::Failed("x".into()),
    ] {
        assert!(retry_offered(&s), "{s:?}");
    }
    for s in [
        SyncState::Synced,
        SyncState::Pushing,
        SyncState::SignedOut,
        SyncState::ReadOnly,
    ] {
        assert!(!retry_offered(&s), "{s:?}");
    }
}

#[gpui_kit::test]
fn a_synced_folder_shows_synced_without_retry(cx: &mut TestAppContext) {
    let fx = team_fx("write");
    let s = team_started(cx, &fx);
    wait_view(cx, &s, "synced", |v| v.state == SyncState::Synced);
    assert_eq!(
        label_of(cx, s.win, "team-status").as_deref(),
        Some("Synced")
    );
    assert!(!present(cx, s.win, "team-status-retry"));
}

#[gpui_kit::test]
fn retry_from_offline_runs_a_cycle_that_lands_synced(cx: &mut TestAppContext) {
    let fx = team_fx("write");
    let s = team_started(cx, &fx);
    wait_view(cx, &s, "synced", |v| v.state == SyncState::Synced);
    show(cx, &s, v(SyncState::Offline, 2));
    assert_eq!(
        label_of(cx, s.win, "team-status").as_deref(),
        Some("Offline, 2 not sent")
    );
    click_id(cx, s.win, "team-status-retry");
    wait_view(cx, &s, "synced again", |v| v.state == SyncState::Synced);
    assert_eq!(
        label_of(cx, s.win, "team-status").as_deref(),
        Some("Synced")
    );
}

#[gpui_kit::test]
fn a_folder_outside_team_mode_has_no_chip(cx: &mut TestAppContext) {
    let fx = team_fx_with("write", false);
    let s = start_on(cx, &fx.ws, Some(fx.hooks()));
    assert!(!present(cx, s.win, "team-status"));
}
