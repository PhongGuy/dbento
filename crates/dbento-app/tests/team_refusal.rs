//! The refusal panel: every finding row in the order received, the overflow row, server
//! errors titled apart, and a retry.
use dbento_app::explorer::part_id;
use dbento_app::team::TeamView;
use dbento_app::team_view::refusal_title;
use dbento_core::{SyncState, parse_refusal};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext};

use crate::support::team::{show, team_fx, team_started, wait_view};
use crate::support::{click_id, label_of, present};

const STDERR: &str = "remote: potluck-reject secret_value refs/heads/main dbento.toml password\n\
remote: potluck-reject secret_value refs/heads/main queries/a.sql password\n\
remote: potluck-reject-more 3\n";

fn refused(text: &str) -> TeamView {
    TeamView {
        state: SyncState::Refused(parse_refusal(text).unwrap()),
        unpushed: 1,
        last_synced: None,
        note: None,
    }
}

#[test]
fn findings_and_server_errors_have_their_own_titles() {
    assert_eq!(
        refusal_title(&parse_refusal(STDERR).unwrap()),
        "The server refused the push. Fix these and save:"
    );
    let err = parse_refusal("remote: potluck-error storage is full\n").unwrap();
    assert_eq!(refusal_title(&err), "The server could not take the push:");
}

#[gpui_kit::test]
fn a_real_refusal_lists_each_finding_and_the_count_left(cx: &mut TestAppContext) {
    let fx = team_fx("write");
    fx.hook(
        "pre-receive",
        &format!(
            "cat >/dev/null\nprintf '%s' '{}' >&2\nexit 1",
            STDERR.replace("remote: ", "")
        ),
    );
    fx.local_commit("queries/a.sql", "-- password hunter2\nselect 1;\n");
    let s = team_started(cx, &fx);
    wait_view(cx, &s, "refused", |v| {
        matches!(v.state, SyncState::Refused(_))
    });
    assert!(present(cx, s.win, "team-refusal"));
    assert_eq!(
        label_of(cx, s.win, part_id("team-refusal-row", "0")).as_deref(),
        Some("dbento.toml on refs/heads/main: a password")
    );
    assert_eq!(
        label_of(cx, s.win, part_id("team-refusal-row", "1")).as_deref(),
        Some("queries/a.sql on refs/heads/main: a password")
    );
    assert_eq!(
        label_of(cx, s.win, part_id("team-refusal-row", "2")).as_deref(),
        Some("and 3 more")
    );
    assert!(!present(cx, s.win, part_id("team-refusal-row", "3")));
    // The committed value never reaches the panel: findings name the rule, not the text.
    let said: Vec<String> = cx
        .update_window(s.win, |_, w, _| {
            w.within("team-refusal")
                .elements()
                .iter()
                .filter_map(|e| e.label().map(str::to_owned))
                .collect()
        })
        .unwrap();
    assert!(!said.is_empty(), "the panel names nothing");
    assert!(said.iter().all(|t| !t.contains("hunter2")), "{said:?}");
    assert_eq!(
        label_of(cx, s.win, "team-status").as_deref(),
        Some("Push refused")
    );
}

#[gpui_kit::test]
fn a_server_error_shows_its_line_under_its_own_title(cx: &mut TestAppContext) {
    let fx = team_fx("write");
    let s = team_started(cx, &fx);
    wait_view(cx, &s, "synced", |v| v.state == SyncState::Synced);
    show(cx, &s, refused("remote: potluck-error storage is full\n"));
    assert_eq!(
        label_of(cx, s.win, "team-refusal-title").as_deref(),
        Some("The server could not take the push:")
    );
    let row = label_of(cx, s.win, part_id("team-refusal-row", "0")).unwrap();
    assert!(row.contains("storage is full"), "{row}");
}

#[gpui_kit::test]
fn retry_after_the_fix_lands_synced_and_hides_the_panel(cx: &mut TestAppContext) {
    let fx = team_fx("write");
    let s = team_started(cx, &fx);
    wait_view(cx, &s, "synced", |v| v.state == SyncState::Synced);
    show(cx, &s, refused(STDERR));
    assert!(present(cx, s.win, "team-refusal"));
    click_id(cx, s.win, "team-refusal-retry");
    wait_view(cx, &s, "synced", |v| v.state == SyncState::Synced);
    assert!(!present(cx, s.win, "team-refusal"));
}
