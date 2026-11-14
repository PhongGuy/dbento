//! Overlays that must not linger or hide each other: startup toasts, tooltips under a
//! dialog, and the explorer's drag chip after a refused drop.
use crate::support::{self, TREE, present, started};
use dbento_app::app::{AppPaths, start};
use dbento_app::explorer::folder_id;
use dbento_core::FolderPath;
use gpui_kit::component::WindowExt as _;
use gpui_kit::test::{TestQueryExt as _, TestWindowExt as _};
use gpui_kit::{
    AppContext as _, InputEvent as _, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    TestAppContext,
};
use std::time::Duration;

#[gpui_kit::test]
fn two_corrupt_startup_files_share_one_readable_toast(cx: &mut TestAppContext) {
    support::init(cx);
    let files = tempfile::tempdir().unwrap();
    let paths = AppPaths::in_dir(files.path());
    std::fs::write(&paths.settings, "{").unwrap();
    std::fs::write(&paths.keymap, "{").unwrap();
    let (win, _) = cx
        .update(|cx| start(support::services(files.path()), None, cx))
        .unwrap();
    let text = support::label_of(cx, win, "notice").unwrap_or_default();
    assert!(
        text.contains("settings.json") && text.contains("keymap.json"),
        "{text:?}"
    );
    let toasts = cx
        .update_window(win, |_, w, cx| {
            w.render_frame(cx);
            w.elements()
                .iter()
                .filter(|e| e.label().is_some_and(|l| l.contains("could not be read")))
                .count()
        })
        .unwrap();
    assert_eq!(toasts, 1, "one toast, not a stack whose lower card hides");
}

#[gpui_kit::test]
fn opening_a_dialog_hides_the_showing_tooltip(cx: &mut TestAppContext) {
    let s = started(cx, TREE);
    cx.update_window(s.win, |_, w, cx| w.hover("rail-settings", cx))
        .unwrap();
    cx.executor().advance_clock(Duration::from_secs(2));
    assert!(
        present(cx, s.win, "tooltip"),
        "the hovered rail shows its tooltip"
    );
    cx.update_window(s.win, |_, w, cx| {
        w.open_dialog(cx, |d, _, _| d.title("Modal"))
    })
    .unwrap();
    assert!(!present(cx, s.win, "tooltip"), "the dialog hides it");
}

#[gpui_kit::test]
fn a_refused_drop_clears_the_drag_chip_at_once(cx: &mut TestAppContext) {
    let s = started(cx, TREE);
    let (team, sub) = (
        folder_id(&"team".parse::<FolderPath>().unwrap()),
        folder_id(&"team/sub".parse::<FolderPath>().unwrap()),
    );
    let dragging = cx
        .update_window(s.win, |_, w, cx| {
            w.render_frame(cx);
            let (from, to) = (
                w.find(team).bounds().center(),
                w.find(sub).bounds().center(),
            );
            let down = MouseDownEvent {
                button: MouseButton::Left,
                position: from,
                modifiers: Default::default(),
                click_count: 1,
                first_mouse: false,
            };
            w.dispatch_event(down.to_platform_input(), cx);
            let moved = MouseMoveEvent {
                position: to,
                pressed_button: Some(MouseButton::Left),
                modifiers: Default::default(),
            };
            w.dispatch_event(moved.to_platform_input(), cx);
            w.render_frame(cx);
            let shown = w.try_find("drag-preview").is_some();
            let up = MouseUpEvent {
                button: MouseButton::Left,
                position: to,
                modifiers: Default::default(),
                click_count: 1,
            };
            w.dispatch_event(up.to_platform_input(), cx);
            shown
        })
        .unwrap();
    assert!(dragging, "the chip shows while dragging");
    // No forced frame here: the chip must go with the frame the release itself asks for.
    cx.run_until_parked();
    let left = cx
        .update_window(s.win, |_, w, _| w.try_find("drag-preview").is_some())
        .unwrap();
    assert!(!left, "the refused drop leaves no chip behind");
}
