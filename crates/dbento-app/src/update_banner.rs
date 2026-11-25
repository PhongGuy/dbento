//! The update banner under the title bar: the offered release, its links, Later and Skip,
//! and Download and restart with its progress, or the armed release with Cancel; and the
//! "What's new" dialog over the offered release's notes.

use dbento_core::update::{
    RELEASES_URL, ReleaseEntry, ReleaseNotes, plain_text, release_notes_url,
};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{
    ActiveTheme as _, Disableable as _, Sizable as _, WindowExt as _, h_flex, v_flex,
};
use gpui_kit::{
    AnyElement, App, ElementId, Entity, FontWeight, Hsla, InteractiveElement as _, IntoElement,
    ParentElement as _, Role, SharedString, StatefulInteractiveElement as _, Styled as _,
    TestSupportExt as _, Window, div, px,
};

use crate::confirm::{DIALOG_WIDTH, dialog_top, footer, frame, title_row};
use crate::shell::Shell;
use crate::theme;
use crate::ui;
use crate::updates::{self, InstallPhase, NotesState, Updates};

/// The banner while the global updater has an offer; None without one. Its actions sit
/// beside the text; in a narrow window a toast in the top right corner may cover the last.
pub fn banner(shell: &Entity<Shell>, _window: &mut Window, cx: &mut App) -> Option<AnyElement> {
    let updates = updates::global(cx)?;
    let u = updates.read(cx);
    let offer = u.offer()?.clone();
    let row = h_flex()
        .id("update-banner")
        .test_support()
        // A narrow window wraps the row onto more lines instead of clipping its actions.
        .min_h(px(32.))
        .py_1()
        .flex_wrap()
        .flex_none()
        .gap_2()
        .px_3()
        .border_b_1()
        .border_color(cx.theme().border)
        .bg(cx.theme().secondary)
        .text_sm();
    // Armed, the release only waits for the quit, so Cancel is the one thing left to do.
    if u.armed() {
        let says = format!("Dbento {} installs when you quit", offer.version);
        let cancel = ui::secondary(Button::new("update-cancel"))
            .small()
            .label("Cancel")
            .on_click(move |_, _, cx| updates.update(cx, |u, cx| u.disarm(cx)));
        let row = row
            .child(label("update-armed", says, cx.theme().foreground))
            .child(cancel);
        return Some(row.into_any_element());
    }
    let (can_install, phase) = (u.can_install(), u.install_phase().clone());
    let state = match &phase {
        InstallPhase::Downloading { received, total } => {
            let says = match received.saturating_mul(100).checked_div(*total) {
                Some(percent) => format!("Downloading {percent}%"),
                None => "Downloading".to_owned(),
            };
            Some(label("update-progress", says, theme::dim(cx)))
        }
        InstallPhase::Failed(message) => Some(label(
            "update-install-error",
            format!("Update failed: {message}"),
            cx.theme().danger,
        )),
        InstallPhase::Idle | InstallPhase::Ready => None,
    };
    let downloading = matches!(phase, InstallPhase::Downloading { .. });
    let whats_new = match u.notes_state() {
        NotesState::Loading | NotesState::Loaded(_) | NotesState::Failed(_) => {
            let updates = updates.clone();
            Some(
                ui::secondary(Button::new("update-whats-new"))
                    .small()
                    .label("What's new")
                    .on_click(move |_, window, cx| whats_new_dialog(updates.clone(), window, cx)),
            )
        }
        NotesState::Unavailable => None,
    };
    let notes = release_notes_url(&offer.version);
    let page = offer.download_page.clone();
    let (later, skip) = (updates.clone(), updates);
    let install = can_install.then(|| {
        let shell = shell.clone();
        Button::new("update-install")
            .small()
            .primary()
            .label("Download and restart")
            .disabled(downloading)
            .on_click(move |_, window, cx| updates::download_and_restart(&shell, window, cx))
    });
    let row = row
        .child(label(
            "update-banner-text",
            format!("Dbento {} is available", offer.version),
            cx.theme().foreground,
        ))
        .children(state)
        .children(whats_new)
        .child(link("update-notes", "Release notes", notes))
        .child(link("update-download-page", "Download page", page))
        .child(
            ui::secondary(Button::new("update-later"))
                .small()
                .label("Later")
                .on_click(move |_, _, cx| later.update(cx, |u, cx| u.later(cx))),
        )
        .child(
            ui::secondary(Button::new("update-skip"))
                .small()
                .label("Skip this version")
                .on_click(move |_, _, cx| skip.update(cx, |u, cx| u.skip(cx))),
        )
        .children(install);
    Some(row.into_any_element())
}

/// A banner text with its own id, read out by its text.
pub(crate) fn label(id: impl Into<ElementId>, says: String, color: Hsla) -> AnyElement {
    let says = SharedString::from(says);
    // The text may shrink below its one-line width, so a narrow row wraps it.
    div()
        .id(id)
        .min_w_0()
        .flex_shrink(1.)
        .text_color(color)
        .role(Role::Label)
        .aria_label(says.clone())
        .test_support()
        .child(says)
        .into_any_element()
}

fn link(id: &'static str, text: &'static str, url: String) -> Button {
    ui::secondary(Button::new(id))
        .small()
        .label(text)
        .on_click(move |_, _, cx| cx.open_url(&url))
}

/// Opens the "What's new" dialog. It reads the notes every frame, so a dialog opened while
/// they load fills in once they land.
fn whats_new_dialog(updates: Entity<Updates>, window: &mut Window, cx: &mut App) {
    window.defer(cx, move |window, cx| {
        window.open_dialog(cx, move |dialog, window, cx| {
            let body: Vec<AnyElement> = match updates.read(cx).notes_state() {
                NotesState::Loading => vec![label(
                    "update-notes-loading",
                    "Loading release notes".to_owned(),
                    theme::dim(cx),
                )],
                NotesState::Failed(_) => vec![label(
                    "update-notes-failed",
                    "Release notes couldn't be loaded".to_owned(),
                    cx.theme().danger,
                )],
                NotesState::Loaded(notes) => notes_body(notes, cx),
                NotesState::Unavailable => Vec::new(),
            };
            let releases = ui::secondary(Button::new("update-notes-releases"))
                .label("Releases page")
                .h(px(32.))
                .on_click(|_, _, cx| cx.open_url(RELEASES_URL));
            let close = ui::secondary(Button::new("update-notes-close"))
                .label("Close")
                .h(px(32.))
                .on_click(|_, window, cx| window.close_dialog(cx));
            frame(dialog, DIALOG_WIDTH, None, cx)
                .margin_top(dialog_top(window))
                .child(
                    v_flex()
                        .id("update-notes-dialog")
                        .aria_label("What's new")
                        .test_support()
                        .child(title_row(None, "What's new", cx))
                        .child(
                            v_flex()
                                .id("update-notes-scroll")
                                .test_support()
                                .max_h(px(360.))
                                .overflow_y_scroll()
                                .gap_2()
                                .px_4()
                                .pt_2()
                                .pb_4()
                                .text_sm()
                                .children(body),
                        )
                        .child(
                            footer(cx)
                                .child(releases)
                                .child(div().flex_1())
                                .child(close),
                        ),
                )
        });
    });
}

// The releases newer than this build, newest first, each with its intro and grouped items;
// items show their pull request number as text, never as a link.
fn notes_body(notes: &ReleaseNotes, cx: &App) -> Vec<AnyElement> {
    let between = notes.between(env!("CARGO_PKG_VERSION"));
    let mut body: Vec<AnyElement> = between
        .releases()
        .iter()
        .enumerate()
        .map(|(r, entry)| release(r, entry, cx))
        .collect();
    if between.older_on_release_page() {
        body.push(label(
            "update-notes-older",
            "Older changes are on the release page".to_owned(),
            theme::dim(cx),
        ));
    }
    body
}

fn release(r: usize, entry: &ReleaseEntry, cx: &App) -> AnyElement {
    let fg = cx.theme().foreground;
    let heading = match entry.date() {
        Some(date) => format!("Dbento {} ({})", shown(entry.version()), shown(date)),
        None => format!("Dbento {}", shown(entry.version())),
    };
    let intro = entry.intro().map(|intro| {
        label(
            SharedString::from(format!("update-notes-intro-{r}")),
            shown(intro),
            theme::dim(cx),
        )
    });
    let groups = entry.groups().iter().enumerate().map(|(g, group)| {
        let items = group.items().iter().enumerate().map(|(i, item)| {
            let says = match item.pr() {
                Some(pr) => format!("\u{2022} {} (#{pr})", shown(item.text())),
                None => format!("\u{2022} {}", shown(item.text())),
            };
            label(
                SharedString::from(format!("update-notes-item-{r}-{g}-{i}")),
                says,
                fg,
            )
        });
        v_flex()
            .gap_1()
            .child(div().font_weight(FontWeight::SEMIBOLD).child(label(
                SharedString::from(format!("update-notes-group-{r}-{g}")),
                shown(group.title()),
                fg,
            )))
            .children(items)
    });
    v_flex()
        .gap_2()
        .pt_2()
        .child(div().font_weight(FontWeight::SEMIBOLD).child(label(
            SharedString::from(format!("update-notes-release-{r}")),
            heading,
            fg,
        )))
        .children(intro)
        .children(groups)
        .into_any_element()
}

// Plain text on one line: line and paragraph separators would break it like a newline.
fn shown(s: &str) -> String {
    plain_text(s).replace(['\u{2028}', '\u{2029}'], " ")
}
