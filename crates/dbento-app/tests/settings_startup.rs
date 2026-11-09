use crate::support;
use std::time::Duration;

use dbento_app::app::{AppPaths, SavedState, start};
use dbento_app::persist::save_json;
use dbento_app::settings_store::{self, SAVE_DELAY};
use dbento_app::theme::Appearance;
use gpui_kit::TestAppContext;
use gpui_kit::component::{Theme, ThemeMode};

#[gpui_kit::test]
fn a_corrupt_file_warns_once_at_startup(cx: &mut TestAppContext) {
    support::init(cx);
    let files = tempfile::tempdir().unwrap();
    std::fs::write(AppPaths::in_dir(files.path()).settings, "{").unwrap();
    let (win, _) = cx
        .update(|cx| start(support::services(files.path()), None, cx))
        .unwrap();
    assert_eq!(
        support::label_of(cx, win, "notice").as_deref(),
        Some(
            "settings.json could not be read and was moved to settings.json.bad; defaults are in use"
        )
    );
}

#[gpui_kit::test]
fn an_existing_settings_file_wins_over_the_state_file(cx: &mut TestAppContext) {
    support::init(cx);
    let files = tempfile::tempdir().unwrap();
    let paths = AppPaths::in_dir(files.path());
    save_json(
        &paths.state,
        &SavedState {
            appearance: Appearance::Light,
            ..SavedState::default()
        },
    )
    .unwrap();
    std::fs::write(&paths.settings, r#"{"appearance":"dark"}"#).unwrap();
    cx.update(|cx| start(support::services(files.path()), None, cx))
        .unwrap();
    cx.run_until_parked();
    assert_eq!(cx.update(|cx| Theme::global(cx).mode), ThemeMode::Dark);
}

#[gpui_kit::test]
fn a_failed_save_shows_a_warning_toast(cx: &mut TestAppContext) {
    support::init(cx);
    let files = tempfile::tempdir().unwrap();
    let (win, _) = cx
        .update(|cx| start(support::services(files.path()), None, cx))
        .unwrap();
    cx.run_until_parked();
    std::fs::create_dir(AppPaths::in_dir(files.path()).settings).unwrap();
    let store = cx.update(|cx| settings_store::store(cx)).unwrap();
    store.update(cx, |s, cx| s.change(cx, |v| v.ui_font.size = 14));
    cx.executor()
        .advance_clock(SAVE_DELAY + Duration::from_millis(1));
    cx.run_until_parked();
    let text = support::label_of(cx, win, "notice").unwrap_or_default();
    assert!(
        text.starts_with("settings.json could not be saved"),
        "{text:?}"
    );
}
