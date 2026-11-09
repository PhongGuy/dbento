//! Opening the Settings dialog from its key, the palette and the rail.

use crate::support;
use crate::support::{click_id, label_of, launched, palette_pick, present};
use dbento_app::app::start;
use dbento_app::keymap::{self, OpenSettings, SETTINGS_KEYS};
use dbento_app::palette::{Mode, PaletteCommand};
use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AnyWindowHandle, AppContext as _, Role, TestAppContext, px};

fn press(cx: &mut TestAppContext, win: AnyWindowHandle, key: &str) {
    cx.update_window(win, |_, w, cx| {
        w.render_frame(cx);
        w.press(key, cx)
    })
    .unwrap();
    cx.run_until_parked();
}

#[gpui_kit::test]
fn the_settings_key_and_command_are_in_the_tables(cx: &mut TestAppContext) {
    assert_eq!(SETTINGS_KEYS, "secondary-,");
    assert_eq!(
        cx.update(|cx| keymap::keys_for(&OpenSettings, cx)),
        Some(SETTINGS_KEYS.into())
    );
    assert_eq!(PaletteCommand::OpenSettings.label(), "Open settings");
    assert_eq!(
        cx.update(|cx| PaletteCommand::OpenSettings.keys(cx)),
        Some(SETTINGS_KEYS.into())
    );
    assert!(PaletteCommand::ALL.contains(&PaletteCommand::OpenSettings));
}

#[gpui_kit::test]
fn the_key_opens_settings_on_the_welcome_view_and_escape_closes_it(cx: &mut TestAppContext) {
    support::init(cx);
    let files = tempfile::tempdir().unwrap();
    let (win, shell) = cx
        .update(|cx| start(support::services(files.path()), None, cx))
        .unwrap();
    cx.run_until_parked();
    press(cx, win, "ctrl-,");
    assert!(present(cx, win, "settings-dialog"));
    press(cx, win, "ctrl-,");
    assert!(
        present(cx, win, "settings-dialog"),
        "a second press keeps the one dialog"
    );
    press(cx, win, "escape");
    assert!(!present(cx, win, "settings-dialog"));
    assert!(shell.read_with(cx, |s, _| s.settings_view().is_none()));
}

#[gpui_kit::test]
fn the_palette_and_the_rail_open_settings(cx: &mut TestAppContext) {
    let o = launched(cx, "", "");
    let shell = o.shell.clone();
    cx.update_window(o.win, |_, w, cx| {
        shell.update(cx, |s, cx| s.open_palette(Mode::All, w, cx))
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(o.win, |_, w, cx| w.input("settings", cx))
        .unwrap();
    palette_pick(cx, o.win, "Open settings");
    assert!(present(cx, o.win, "settings-dialog"));
    click_id(cx, o.win, "settings-close");
    assert!(!present(cx, o.win, "settings-dialog"));
    let role = cx
        .update_window(o.win, |_, w, cx| {
            w.render_frame(cx);
            w.find("rail-settings").role()
        })
        .unwrap();
    assert_eq!(role, Some(Role::Button));
    assert_eq!(
        label_of(cx, o.win, "rail-settings").as_deref(),
        Some("Settings")
    );
    click_id(cx, o.win, "rail-settings");
    assert!(present(cx, o.win, "settings-dialog"));
}

#[gpui_kit::test]
fn the_dialog_theme_switches_the_window_and_keeps_the_fonts(cx: &mut TestAppContext) {
    let o = launched(cx, "", "");
    click_id(cx, o.win, "rail-settings");
    let store = cx
        .update(|cx| dbento_app::settings_store::store(cx))
        .unwrap();
    store.update(cx, |s, cx| s.change(cx, |v| v.ui_font.size = 15));
    cx.run_until_parked();
    for (id, want) in [
        ("settings-theme-light", ThemeMode::Light),
        ("settings-theme-dark", ThemeMode::Dark),
    ] {
        click_id(cx, o.win, id);
        let (mode, size) = cx.update(|cx| {
            let t = Theme::global(cx);
            (t.mode, t.font_size)
        });
        assert_eq!((mode, size), (want, px(15.)), "{id}");
    }
}

#[gpui_kit::test]
fn escape_in_the_font_picker_closes_only_the_list(cx: &mut TestAppContext) {
    let o = launched(cx, "", "");
    click_id(cx, o.win, "rail-settings");
    click_id(cx, o.win, "settings-ui-font-family");
    assert!(present(cx, o.win, "settings-ui-font-search"));
    press(cx, o.win, "escape");
    assert!(
        !present(cx, o.win, "settings-ui-font-search"),
        "the list closed"
    );
    assert!(present(cx, o.win, "settings-dialog"), "the dialog stayed");
    press(cx, o.win, "escape");
    assert!(!present(cx, o.win, "settings-dialog"));
}

#[gpui_kit::test]
fn settings_wait_while_another_dialog_is_open(cx: &mut TestAppContext) {
    let o = launched(cx, "", "");
    click_id(cx, o.win, "connection-new");
    assert!(present(cx, o.win, "conn-name"));
    let shell = o.shell.clone();
    cx.update_window(o.win, |_, w, cx| {
        shell.update(cx, |s, cx| s.open_settings(w, cx))
    })
    .unwrap();
    cx.run_until_parked();
    assert!(!present(cx, o.win, "settings-dialog"));
    assert!(
        present(cx, o.win, "conn-name"),
        "the open dialog is left alone"
    );
}
