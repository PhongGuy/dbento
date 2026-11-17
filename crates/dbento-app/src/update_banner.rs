//! The update banner under the title bar: the offered release, its links, Later and Skip,
//! and Download and restart with its progress, or the armed release with Cancel.

use dbento_core::update::release_notes_url;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{ActiveTheme as _, Disableable as _, Sizable as _, h_flex};
use gpui_kit::{
    AnyElement, App, Entity, Hsla, InteractiveElement as _, IntoElement, ParentElement as _, Role,
    SharedString, StatefulInteractiveElement as _, Styled as _, TestSupportExt as _, Window, div,
    px,
};

use crate::shell::Shell;
use crate::theme;
use crate::ui;
use crate::updates::{self, InstallPhase};

/// The banner while the global updater has an offer; None without one. Its actions sit
/// beside the text, clear of the toasts that stack in the window's top right corner.
pub fn banner(shell: &Entity<Shell>, _window: &mut Window, cx: &mut App) -> Option<AnyElement> {
    let updates = updates::global(cx)?;
    let u = updates.read(cx);
    let offer = u.offer()?.clone();
    let row = h_flex()
        .id("update-banner")
        .test_support()
        .h(px(32.))
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

fn label(id: &'static str, says: String, color: Hsla) -> AnyElement {
    let says = SharedString::from(says);
    div()
        .id(id)
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
