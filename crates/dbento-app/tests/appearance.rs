mod support;
use dbento_app::app::{AppPaths, SavedState, start};
use dbento_app::persist::{load_json, save_json};
use dbento_app::settings_store::SAVE_DELAY;
use dbento_app::theme::{self, Appearance, mode_for};
use gpui_kit::component::{Theme, ThemeMode};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AnyWindowHandle, AppContext as _, TestAppContext, WindowAppearance};
use std::path::Path;

fn mode(cx: &mut TestAppContext) -> ThemeMode {
    cx.update(|cx| Theme::global(cx).mode)
}
fn saved(files: &Path) -> SavedState {
    load_json(&AppPaths::in_dir(files).state).value
}
fn save(files: &Path, state: SavedState) {
    save_json(&AppPaths::in_dir(files).state, &state).unwrap();
}
fn started(cx: &mut TestAppContext, files: &Path, dir: Option<&Path>) -> AnyWindowHandle {
    let (win, _) = cx
        .update(|cx| start(support::services(files), dir.map(Path::to_path_buf), cx))
        .unwrap();
    cx.run_until_parked();
    win
}
fn shortcut(cx: &mut TestAppContext, win: AnyWindowHandle) {
    cx.update_window(win, |_, w, cx| {
        w.render_frame(cx);
        w.press("ctrl-k", cx);
        w.press("ctrl-t", cx);
    })
    .unwrap();
    cx.run_until_parked();
}
#[test]
fn system_follows_the_os_and_a_fixed_choice_overrides_it() {
    use WindowAppearance as W;
    assert_eq!(mode_for(Appearance::System, W::Dark), ThemeMode::Dark);
    assert_eq!(
        mode_for(Appearance::System, W::VibrantDark),
        ThemeMode::Dark
    );
    assert_eq!(mode_for(Appearance::System, W::Light), ThemeMode::Light);
    assert_eq!(mode_for(Appearance::Dark, W::Light), ThemeMode::Dark);
    assert_eq!(mode_for(Appearance::Light, W::Dark), ThemeMode::Light);
    assert_eq!(Appearance::System.next(), Appearance::Dark);
    assert_eq!(Appearance::Dark.next(), Appearance::Light);
    assert_eq!(Appearance::Light.next(), Appearance::System);
}
#[test]
fn system_is_the_default_and_an_older_state_file_still_loads() {
    assert_eq!(SavedState::default().appearance, Appearance::System);
    let old: SavedState = serde_json::from_str(r#"{"last_workspace":"/w"}"#).unwrap();
    assert_eq!(
        (old.last_workspace.as_deref(), old.appearance),
        (Some(Path::new("/w")), Appearance::System)
    );
    let dark: SavedState =
        serde_json::from_str(r#"{"last_workspace":null,"appearance":"dark"}"#).unwrap();
    assert_eq!(dark.appearance, Appearance::Dark);
}
#[gpui_kit::test]
fn a_fresh_start_follows_the_window_appearance(cx: &mut TestAppContext) {
    support::init(cx);
    cx.update(|cx| theme::set_mode(ThemeMode::Dark, None, cx));
    let files = tempfile::tempdir().unwrap();
    started(cx, files.path(), None);
    assert_eq!(
        mode(cx),
        ThemeMode::Light,
        "the test window reports a light system"
    );
    assert!(
        !AppPaths::in_dir(files.path()).state.exists(),
        "following the system writes nothing"
    );
}
fn settings_appearance(cx: &mut TestAppContext, files: &Path) -> Appearance {
    cx.executor()
        .advance_clock(SAVE_DELAY + std::time::Duration::from_millis(1));
    cx.run_until_parked();
    load_json::<dbento_app::settings::Settings>(&AppPaths::in_dir(files).settings)
        .value
        .appearance
}
#[gpui_kit::test]
fn the_shortcut_switches_and_saves_each_choice(cx: &mut TestAppContext) {
    support::init(cx);
    let files = tempfile::tempdir().unwrap();
    let win = started(cx, files.path(), None);
    assert!(support::present(cx, win, "welcome-open"));
    for (choice, want) in [
        (Appearance::Dark, ThemeMode::Dark),
        (Appearance::Light, ThemeMode::Light),
        (Appearance::System, ThemeMode::Light),
    ] {
        shortcut(cx, win);
        assert_eq!(
            (mode(cx), settings_appearance(cx, files.path())),
            (want, choice)
        );
        assert_eq!(
            saved(files.path()).appearance,
            Appearance::System,
            "state.json is never changed again"
        );
    }
}
#[gpui_kit::test]
fn the_shortcut_works_while_the_editor_has_focus(cx: &mut TestAppContext) {
    let o = support::opened(cx, "", "", 0);
    shortcut(cx, o.win);
    assert_eq!(
        (mode(cx), settings_appearance(cx, o.files.path())),
        (ThemeMode::Dark, Appearance::Dark)
    );
    assert_eq!(
        saved(o.files.path()).last_workspace.as_deref(),
        Some(o.fx.dir.path()),
        "the workspace survives"
    );
}
#[gpui_kit::test]
fn a_font_change_survives_the_appearance_shortcut(cx: &mut TestAppContext) {
    support::init(cx);
    let files = tempfile::tempdir().unwrap();
    let win = started(cx, files.path(), None);
    let store = cx
        .update(|cx| dbento_app::settings_store::store(cx))
        .unwrap();
    store.update(cx, |s, cx| s.change(cx, |v| v.ui_font.size = 15));
    cx.run_until_parked();
    shortcut(cx, win);
    assert_eq!(
        cx.update(|cx| Theme::global(cx).font_size),
        gpui_kit::px(15.)
    );
}
#[gpui_kit::test]
fn opening_a_workspace_keeps_the_saved_choice(cx: &mut TestAppContext) {
    support::init(cx);
    let (fx, files) = (support::fixture("", ""), tempfile::tempdir().unwrap());
    save(
        files.path(),
        SavedState {
            appearance: Appearance::Light,
            ..SavedState::default()
        },
    );
    let win = started(cx, files.path(), Some(fx.dir.path()));
    assert!(support::present(cx, win, "connection-list"));
    let s = saved(files.path());
    assert_eq!(
        (s.last_workspace.as_deref(), s.appearance),
        (Some(fx.dir.path()), Appearance::Light)
    );
}
#[gpui_kit::test]
fn forgetting_a_broken_workspace_keeps_the_saved_choice(cx: &mut TestAppContext) {
    support::init(cx);
    let (empty, files) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    save(
        files.path(),
        SavedState {
            last_workspace: Some(empty.path().into()),
            appearance: Appearance::Dark,
            ..SavedState::default()
        },
    );
    let win = started(cx, files.path(), None);
    assert!(support::present(cx, win, "welcome-error"));
    assert_eq!(
        mode(cx),
        ThemeMode::Dark,
        "the saved choice is restored at start"
    );
    let s = saved(files.path());
    assert_eq!((s.last_workspace, s.appearance), (None, Appearance::Dark));
}
#[gpui_kit::test]
fn an_os_change_moves_the_theme_only_under_system(cx: &mut TestAppContext) {
    support::init(cx);
    let files = tempfile::tempdir().unwrap();
    let win = started(cx, files.path(), None);
    use WindowAppearance as W;
    for (choice, os, want) in [
        (Appearance::System, W::Dark, ThemeMode::Dark),
        (Appearance::System, W::Light, ThemeMode::Light),
        (Appearance::Light, W::Dark, ThemeMode::Light),
        (Appearance::Dark, W::Light, ThemeMode::Dark),
    ] {
        cx.update_window(win, |_, w, cx| theme::apply(choice, os, w, cx))
            .unwrap();
        assert_eq!(mode(cx), want, "{choice:?} under {os:?}");
    }
}
