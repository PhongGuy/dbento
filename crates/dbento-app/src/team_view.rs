//! What the app shows of a team sync: the status-bar chip with its retry, and the panel
//! listing why the server refused a push.

use dbento_core::{Refusal, SyncState};
use gpui_kit::component::button::Button;
use gpui_kit::component::{ActiveTheme as _, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, ElementId, Entity, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, Role, SharedString, StatefulInteractiveElement as _, Styled as _,
    TestSupportExt as _, div, px,
};

use crate::explorer::part_id;
use crate::team::{TeamSession, TeamView};
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
/// retry_offered, a `team-status-retry` button that calls `TeamSession::retry`.
pub fn chip(session: &Entity<TeamSession>, cx: &App) -> AnyElement {
    let view = session.read(cx).view();
    let says = chip_label(view);
    let detail = view.and_then(chip_detail);
    let retry = view.is_some_and(|v| retry_offered(&v.state));
    let mut chip = ui::chip_base(cx)
        .id("team-status")
        .aria_label(says.clone())
        .child(says);
    if let Some(detail) = detail {
        chip = chip.tooltip(move |window, cx| theme::tooltip(detail.clone(), window, cx));
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
        .map(|(ix, row)| label_keyed(part_id("team-refusal-row", &ix.to_string()), row, fg));
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

// The banner label under a keyed id, since rows are numbered rather than named.
fn label_keyed(id: ElementId, says: String, color: Hsla) -> AnyElement {
    let says = SharedString::from(says);
    div()
        .id(id)
        .min_w_0()
        .text_color(color)
        .role(Role::Label)
        .aria_label(says.clone())
        .test_support()
        .child(says)
        .into_any_element()
}
