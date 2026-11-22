//! What the app shows of a team sync: the status-bar chip with its retry, and the panel
//! listing why the server refused a push, and a banner per conflicted file.

use dbento_core::{ConflictKind, Refusal, SyncState};
use gpui_kit::component::button::Button;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, ElementId, Entity, InteractiveElement as _, IntoElement, ParentElement as _,
    Role, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, px,
};

use crate::explorer::part_id;
use crate::shell::Shell;
use crate::team::{TeamSession, TeamView};
use crate::team_dialog::TeamDialogMode;
use crate::update_banner::label;
use crate::{theme, ui};

/// The chip's text for `view`; "Connecting" before the sync has reported.
pub fn chip_label(view: Option<&TeamView>) -> String {
    let Some(view) = view else {
        return "Connecting".to_owned();
    };
    // Where saves pile up locally, the chip says how many have not reached the server.
    let with_unsent = |word: &str| match view.unpushed {
        0 => word.to_owned(),
        n => format!("{word}, {n} not sent"),
    };
    #[allow(
        clippy::wildcard_enum_match_arm,
        reason = "SyncState is non_exhaustive; a state added later reads as a plain sync"
    )]
    match &view.state {
        SyncState::Synced => "Synced".to_owned(),
        SyncState::Committing => "Saving".to_owned(),
        SyncState::Pulling => "Pulling".to_owned(),
        SyncState::Pushing => "Pushing".to_owned(),
        SyncState::Offline => with_unsent("Offline"),
        SyncState::ReadOnly if view.note.is_some() => "Read only, not following".to_owned(),
        SyncState::ReadOnly => "Read only".to_owned(),
        SyncState::SignedOut => with_unsent("Signed out"),
        SyncState::GitMissing => "Git not found".to_owned(),
        SyncState::Busy => "Busy".to_owned(),
        SyncState::Refused(_) => "Push refused".to_owned(),
        SyncState::Conflicted(files) if files.len() == 1 => "Conflict in 1 file".to_owned(),
        SyncState::Conflicted(files) => format!("Conflicts in {} files", files.len()),
        SyncState::Failed(_) => "Sync failed".to_owned(),
        _ => "Syncing".to_owned(),
    }
}

/// The chip's tooltip: a failure's reason, or why a read-only folder is not following.
pub fn chip_detail(view: &TeamView) -> Option<String> {
    #[allow(
        clippy::wildcard_enum_match_arm,
        reason = "only two states carry a detail; every other one has none"
    )]
    match &view.state {
        SyncState::Failed(why) => Some(why.clone()),
        SyncState::ReadOnly => view.note.clone(),
        _ => None,
    }
}

/// Whether a retry can help: the server, the lock or git may be back, or the failure passed.
pub fn retry_offered(state: &SyncState) -> bool {
    matches!(
        state,
        SyncState::Offline | SyncState::Busy | SyncState::Failed(_) | SyncState::GitMissing
    )
}

/// `team-status` (ui::chip, named by its label, tooltip from chip_detail) and, when
/// retry_offered, a `team-status-retry` button that calls `TeamSession::retry`. While signed
/// out, clicking the chip opens the sign-in dialog for the binding's account.
pub fn chip(shell: &Entity<Shell>, session: &Entity<TeamSession>, cx: &App) -> AnyElement {
    let view = session.read(cx).view();
    let says = chip_label(view);
    let detail = view.and_then(chip_detail);
    let retry = view.is_some_and(|v| retry_offered(&v.state));
    let signed_out = view.is_some_and(|v| v.state == SyncState::SignedOut);
    let mut chip = ui::chip_base(cx)
        .id("team-status")
        .aria_label(says.clone())
        .child(says);
    if let Some(detail) = detail {
        chip = chip.tooltip(move |window, cx| theme::tooltip(detail.clone(), window, cx));
    }
    if signed_out {
        let record = session.read(cx).record();
        let mode = TeamDialogMode::SignIn {
            server: record.server.clone(),
            username: record.username.clone(),
        };
        let shell = shell.clone();
        chip = chip.cursor_pointer().on_click(move |_, window, cx| {
            shell.update(cx, |s, cx| s.open_team_dialog(mode.clone(), window, cx));
        });
    }
    let retry = retry.then(|| {
        let session = session.clone();
        ui::secondary(Button::new("team-status-retry"))
            .xsmall()
            .label("Retry")
            .on_click(move |_, _, cx| session.read(cx).retry())
    });
    h_flex()
        .flex_none()
        .gap_1()
        .child(chip.test_support())
        .children(retry)
        .into_any_element()
}

/// The panel's heading: a refusal of findings asks for fixes; a server failure only reports.
pub fn refusal_title(refusal: &Refusal) -> &'static str {
    if refusal.is_server_error() {
        "The server could not take the push:"
    } else {
        "The server refused the push. Fix these and save:"
    }
}

/// While the state is Refused: `team-refusal` (Role::Alert), a title `team-refusal-title`,
/// one label per `Refusal::rows()` entry in order with id `part_id("team-refusal-row", "<ix>")`,
/// and `team-refusal-retry` calling `TeamSession::retry`. None otherwise.
pub fn refusal_panel(session: &Entity<TeamSession>, cx: &App) -> Option<AnyElement> {
    let SyncState::Refused(refusal) = &session.read(cx).view()?.state else {
        return None;
    };
    let fg = cx.theme().foreground;
    let rows = refusal
        .rows()
        .into_iter()
        .enumerate()
        .map(|(ix, row)| label(part_id("team-refusal-row", &ix.to_string()), row, fg));
    let session = session.clone();
    let retry = ui::secondary(Button::new("team-refusal-retry"))
        .small()
        .label("Retry")
        .on_click(move |_, _, cx| session.read(cx).retry());
    let panel = h_flex()
        .id("team-refusal")
        .role(Role::Alert)
        .test_support()
        .min_h(px(32.))
        .py_1()
        .flex_none()
        .items_start()
        .gap_2()
        .px_3()
        .border_b_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().secondary)
        .text_sm()
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_0p5()
                .child(label(
                    "team-refusal-title",
                    refusal_title(refusal).to_owned(),
                    cx.theme().danger,
                ))
                .children(rows),
        )
        .child(retry);
    Some(panel.into_any_element())
}

/// The banner line for conflicted `path`: where the server's side and mine each went.
pub fn conflict_text(path: &str, kind: ConflictKind) -> String {
    #[allow(
        clippy::wildcard_enum_match_arm,
        reason = "ConflictKind is non_exhaustive; a kind added later reads as both changed"
    )]
    match kind {
        ConflictKind::DeletedLocally => format!(
            "{path} was deleted here and changed on the server. The server's version is back."
        ),
        ConflictKind::DeletedRemotely => {
            format!("{path} was deleted on the server and changed here. Yours is saved.")
        }
        _ => format!(
            "{path} changed here and on the server. The server's version is open; yours is saved."
        ),
    }
}

/// One row per conflicted file while the state is Conflicted, all keyed by path with
/// `part_id`: row `team-conflict`, text `team-conflict-text`, buttons `team-conflict-reload`,
/// `team-conflict-keep`, `team-conflict-compare`.
pub fn conflict_banners(
    shell: &Entity<Shell>,
    session: &Entity<TeamSession>,
    cx: &App,
) -> Vec<AnyElement> {
    let Some(SyncState::Conflicted(files)) = session.read(cx).view().map(|v| &v.state) else {
        return Vec::new();
    };
    let theme = cx.theme();
    files
        .iter()
        .map(|file| {
            let path = file.path.clone();
            // Reload keeps the server's file already on disk and drops the saved copy; Keep
            // mine puts the copy back and pushes it. Either way the settle rereads open tabs.
            let reload = {
                let (session, path) = (session.clone(), path.clone());
                button(part_id("team-conflict-reload", &path), "Reload")
                    .on_click(move |_, _, cx| session.read(cx).take_theirs(&path))
            };
            let keep = {
                let (session, path) = (session.clone(), path.clone());
                button(part_id("team-conflict-keep", &path), "Keep mine")
                    .on_click(move |_, _, cx| session.read(cx).keep_mine(&path))
            };
            let compare = {
                let (shell, path) = (shell.clone(), path.clone());
                button(part_id("team-conflict-compare", &path), "Compare").on_click(
                    move |_, window, cx| {
                        shell.update(cx, |s, cx| s.ask_team_compare(path.clone(), window, cx));
                    },
                )
            };
            h_flex()
                .id(part_id("team-conflict", &path))
                .role(Role::Status)
                .test_support()
                // A narrow window wraps the row onto more lines instead of clipping its actions.
                .min_h(px(32.))
                .py_1()
                .flex_wrap()
                .flex_none()
                .gap_2()
                .px_3()
                .border_b_1()
                .border_color(theme.border)
                .bg(theme.secondary)
                .text_sm()
                .child(label(
                    part_id("team-conflict-text", &path),
                    conflict_text(&path, file.kind),
                    theme.foreground,
                ))
                .child(reload)
                .child(compare)
                .child(keep)
                .into_any_element()
        })
        .collect()
}

fn button(id: ElementId, text: &'static str) -> Button {
    ui::secondary(Button::new(id)).small().label(text)
}
