use crate::support;
use crate::support::{click_id, label_of, present};
use dbento_app::app::AppPaths;
use dbento_app::settings::{Density, Settings};
use dbento_app::settings_dialog::{
    FontTarget, SettingsView, SettingsViewEvent, family_list, filter_families, is_installed,
};
use dbento_app::settings_store::{self, SAVE_DELAY, SettingsStore};
use dbento_app::theme::Appearance;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    AnyWindowHandle, AppContext as _, Bounds, Entity, Role, TestAppContext, WindowBounds,
    WindowOptions, point, px, size,
};
use std::time::Duration;

struct D {
    files: tempfile::TempDir,
    win: AnyWindowHandle,
    view: Entity<SettingsView>,
    store: Entity<SettingsStore>,
}
fn dialog_over(cx: &mut TestAppContext, with_path: bool, seed_file: Option<&str>) -> D {
    // support::init runs theme::install, which registers the bundled Inter and JetBrains Mono.
    support::init(cx);
    let files = tempfile::tempdir().unwrap();
    let path = AppPaths::in_dir(files.path()).settings;
    if let Some(text) = seed_file {
        std::fs::write(&path, text).unwrap();
    }
    let store =
        cx.update(|cx| settings_store::install(with_path.then_some(path), Appearance::System, cx));
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
            point(px(0.), px(0.)),
            size(px(640.), px(1000.)),
        ))),
        ..Default::default()
    };
    let (win, view) = cx
        .update(|cx| {
            gpui_kit::open_window(options, cx, |w, cx| cx.new(|cx| SettingsView::new(w, cx)))
        })
        .unwrap();
    cx.run_until_parked();
    D {
        files,
        win,
        view,
        store,
    }
}
fn dialog(cx: &mut TestAppContext) -> D {
    dialog_over(cx, true, None)
}
fn get(cx: &mut TestAppContext, d: &D) -> Settings {
    d.store.read_with(cx, |s, _| s.get().clone())
}
fn focus_size(cx: &mut TestAppContext, d: &D, t: FontTarget) {
    let input = d.view.read_with(cx, |v, _| v.size_input(t).clone());
    cx.update_window(d.win, |_, w, cx| input.update(cx, |s, cx| s.focus(w, cx)))
        .unwrap();
    cx.run_until_parked();
}
fn press(cx: &mut TestAppContext, d: &D, key: &str) {
    cx.update_window(d.win, |_, w, cx| w.press(key, cx))
        .unwrap();
    cx.run_until_parked();
}
fn type_text(cx: &mut TestAppContext, d: &D, text: &str) {
    cx.update_window(d.win, |_, w, cx| w.input(text, cx))
        .unwrap();
    cx.run_until_parked();
}
fn size_text(cx: &mut TestAppContext, d: &D, t: FontTarget) -> String {
    d.view
        .read_with(cx, |v, cx| v.size_input(t).read(cx).value().to_string())
}

#[test]
fn the_bundled_families_come_first_and_search_ignores_case() {
    let list = family_list(&[
        "Zeta".into(),
        "Inter".into(),
        "Alpha".into(),
        "Alpha".into(),
    ]);
    assert_eq!(list, ["Inter", "JetBrains Mono", "Alpha", "Zeta"]);
    assert_eq!(filter_families(&list, ""), list);
    assert_eq!(filter_families(&list, "AL"), ["Alpha"]);
    assert_eq!(filter_families(&list, "mono"), ["JetBrains Mono"]);
    assert!(is_installed("Inter", &[]) && is_installed("Zeta", &["Zeta".into()]));
    assert!(!is_installed("Nope", &["Zeta".into()]));
}

#[gpui_kit::test]
fn every_control_carries_its_id_role_and_label(cx: &mut TestAppContext) {
    let d = dialog(cx);
    click_id(cx, d.win, "settings-ui-font-family");
    let ids = [
        "settings-dialog",
        "settings-theme-system",
        "settings-theme-dark",
        "settings-theme-light",
        "settings-density-default",
        "settings-density-compact",
        "settings-ui-font-family",
        "settings-editor-font-family",
        "settings-grid-font-family",
        "settings-ui-font-search",
        "settings-ui-font-option-0",
        "settings-ui-font-size",
        "settings-editor-font-size",
        "settings-grid-font-size",
        "settings-reset",
        "settings-close",
    ];
    for id in ids {
        let role = cx
            .update_window(d.win, |_, w, cx| {
                w.render_frame(cx);
                w.try_find(id).unwrap_or_else(|| panic!("no {id}")).role()
            })
            .unwrap();
        assert!(
            role.is_some_and(|r| r != Role::GenericContainer),
            "{id}: {role:?}"
        );
    }
    assert_eq!(
        label_of(cx, d.win, "settings-dialog").as_deref(),
        Some("Settings")
    );
    assert_eq!(
        label_of(cx, d.win, "settings-editor-font-family").as_deref(),
        Some("Editor font family: JetBrains Mono")
    );
    assert_eq!(
        label_of(cx, d.win, "settings-grid-font-size").as_deref(),
        Some("Grid font size")
    );
    assert!(!present(cx, d.win, "settings-not-saved"));
}

#[gpui_kit::test]
fn theme_and_density_segments_change_the_store_and_the_file(cx: &mut TestAppContext) {
    let d = dialog(cx);
    click_id(cx, d.win, "settings-theme-dark");
    click_id(cx, d.win, "settings-density-compact");
    let s = get(cx, &d);
    assert_eq!(
        (s.appearance, s.density),
        (Appearance::Dark, Density::Compact)
    );
    let selected = cx
        .update_window(d.win, |_, w, cx| {
            w.render_frame(cx);
            w.find("settings-density-compact").selected()
        })
        .unwrap();
    assert_eq!(selected, Some(true));
    cx.executor()
        .advance_clock(SAVE_DELAY + Duration::from_millis(1));
    cx.run_until_parked();
    let text = std::fs::read_to_string(AppPaths::in_dir(d.files.path()).settings).unwrap();
    assert!(
        text.contains("\"compact\"") && text.contains("\"dark\""),
        "{text}"
    );
}

#[gpui_kit::test]
fn the_family_picker_searches_and_picks(cx: &mut TestAppContext) {
    let d = dialog(cx);
    click_id(cx, d.win, "settings-ui-font-family");
    assert_eq!(
        d.view.read_with(cx, |v, _| v.open_picker()),
        Some(FontTarget::Ui)
    );
    assert_eq!(
        label_of(cx, d.win, "settings-ui-font-option-0").as_deref(),
        Some("Inter"),
        "bundled first"
    );
    type_text(cx, &d, "jetb");
    assert_eq!(
        label_of(cx, d.win, "settings-ui-font-option-0").as_deref(),
        Some("JetBrains Mono")
    );
    click_id(cx, d.win, "settings-ui-font-option-0");
    assert_eq!(get(cx, &d).ui_font.family, "JetBrains Mono");
    assert!(
        !present(cx, d.win, "settings-ui-font-search"),
        "a pick closes the list"
    );
    assert_eq!(
        label_of(cx, d.win, "settings-ui-font-family").as_deref(),
        Some("UI font family: JetBrains Mono")
    );
}

#[gpui_kit::test]
fn a_typed_size_out_of_range_is_clamped(cx: &mut TestAppContext) {
    let d = dialog(cx);
    focus_size(cx, &d, FontTarget::Ui);
    press(cx, &d, "ctrl-a");
    type_text(cx, &d, "40");
    assert_eq!(get(cx, &d).ui_font.size, 16, "UI stops at 16");
    press(cx, &d, "enter");
    assert_eq!(
        size_text(cx, &d, FontTarget::Ui),
        "16",
        "Enter shows the clamped value"
    );
    focus_size(cx, &d, FontTarget::Grid);
    press(cx, &d, "ctrl-a");
    type_text(cx, &d, "9");
    assert_eq!(get(cx, &d).grid_font.size, 10, "grid starts at 10");
    press(cx, &d, "enter");
    assert_eq!(size_text(cx, &d, FontTarget::Grid), "10");
}

#[gpui_kit::test]
fn the_step_buttons_change_a_size_within_its_range(cx: &mut TestAppContext) {
    let d = dialog(cx);
    let step = |cx: &mut TestAppContext, field: &'static str, button: &'static str| {
        cx.update_window(d.win, |_, w, cx| {
            w.render_frame(cx);
            w.within(field).click(button, cx)
        })
        .unwrap();
        cx.run_until_parked();
    };
    step(cx, "settings-grid-font-size", "increment");
    assert_eq!(
        (
            get(cx, &d).grid_font.size,
            size_text(cx, &d, FontTarget::Grid)
        ),
        (13, "13".to_owned())
    );
    for _ in 0..5 {
        step(cx, "settings-ui-font-size", "increment");
    }
    assert_eq!(get(cx, &d).ui_font.size, 16, "UI stops at 16");
    step(cx, "settings-editor-font-size", "decrement");
    assert_eq!(get(cx, &d).editor_font.size, 11);
}

#[gpui_kit::test]
fn up_and_down_step_a_focused_size(cx: &mut TestAppContext) {
    let d = dialog(cx);
    focus_size(cx, &d, FontTarget::Grid);
    press(cx, &d, "up");
    assert_eq!(get(cx, &d).grid_font.size, 13);
    press(cx, &d, "down");
    press(cx, &d, "down");
    assert_eq!(
        (
            get(cx, &d).grid_font.size,
            size_text(cx, &d, FontTarget::Grid)
        ),
        (11, "11".to_owned())
    );
}

#[gpui_kit::test]
fn typing_a_size_applies_and_is_not_rewritten_midway(cx: &mut TestAppContext) {
    let d = dialog(cx);
    focus_size(cx, &d, FontTarget::Ui);
    press(cx, &d, "ctrl-a");
    type_text(cx, &d, "1");
    assert_eq!(
        size_text(cx, &d, FontTarget::Ui),
        "1",
        "a partial value stays as typed"
    );
    type_text(cx, &d, "5");
    assert_eq!(
        (size_text(cx, &d, FontTarget::Ui), get(cx, &d).ui_font.size),
        ("15".to_owned(), 15)
    );
}

#[gpui_kit::test]
fn an_outside_change_shows_in_the_dialog(cx: &mut TestAppContext) {
    let d = dialog(cx);
    d.store.update(cx, |s, cx| {
        s.change(cx, |v| {
            v.grid_font.size = 18;
            v.editor_font.family = "No Such Font 9".into();
        })
    });
    cx.run_until_parked();
    assert_eq!(size_text(cx, &d, FontTarget::Grid), "18");
    assert_eq!(
        label_of(cx, d.win, "settings-font-not-installed-editor").as_deref(),
        Some("not installed")
    );
    assert!(!present(cx, d.win, "settings-font-not-installed-ui"));
}

#[gpui_kit::test]
fn reset_and_close(cx: &mut TestAppContext) {
    let d = dialog_over(
        cx,
        true,
        Some(r#"{"future":1,"density":"compact","grid_font":{"size":16}}"#),
    );
    let closes = std::rc::Rc::new(std::cell::Cell::new(0));
    let seen = closes.clone();
    cx.update(|cx| {
        cx.subscribe(&d.view, move |_, e: &SettingsViewEvent, _| match e {
            SettingsViewEvent::Close => seen.set(seen.get() + 1),
        })
        .detach()
    });
    click_id(cx, d.win, "settings-reset");
    let s = get(cx, &d);
    assert_eq!((s.density, s.grid_font.size), (Density::Default, 12));
    assert_eq!(s.extra().get("future"), Some(&serde_json::json!(1)));
    assert_eq!(size_text(cx, &d, FontTarget::Grid), "12");
    click_id(cx, d.win, "settings-close");
    assert_eq!(closes.get(), 1);
}

#[gpui_kit::test]
fn an_unsaved_session_says_so(cx: &mut TestAppContext) {
    let d = dialog_over(cx, false, None);
    assert_eq!(
        label_of(cx, d.win, "settings-not-saved").as_deref(),
        Some("not saved: no config folder")
    );
}
