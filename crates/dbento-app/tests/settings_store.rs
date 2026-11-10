use crate::support;
use dbento_app::app::AppPaths;
use dbento_app::settings::{Density, Settings};
use dbento_app::settings_store::{self, SAVE_DELAY, SettingsEvent, SettingsStore};
use dbento_app::theme::Appearance;
use gpui_kit::component::Theme;
use gpui_kit::{Entity, TestAppContext, px};
use std::cell::Cell;
use std::path::Path;
use std::rc::Rc;
use std::time::Duration;

fn installed(cx: &mut TestAppContext, path: Option<&Path>) -> Entity<SettingsStore> {
    support::init(cx);
    let s = cx
        .update(|cx| settings_store::install(path.map(Path::to_path_buf), Appearance::System, cx));
    cx.run_until_parked();
    s
}
fn change(cx: &mut TestAppContext, s: &Entity<SettingsStore>, f: impl FnOnce(&mut Settings)) {
    s.update(cx, |s, cx| s.change(cx, f));
    cx.run_until_parked();
}
fn wait_save(cx: &mut TestAppContext) {
    cx.executor()
        .advance_clock(SAVE_DELAY + Duration::from_millis(1));
    cx.run_until_parked();
}
fn on_disk(path: &Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}
fn count_failures(cx: &mut TestAppContext, s: &Entity<SettingsStore>) -> Rc<Cell<u32>> {
    let failures = Rc::new(Cell::new(0));
    let seen = Rc::clone(&failures);
    cx.update(|cx| {
        cx.subscribe(s, move |_, e: &SettingsEvent, _| match e {
            SettingsEvent::SaveFailed(_) => seen.set(seen.get() + 1),
        })
        .detach()
    });
    failures
}

#[gpui_kit::test]
fn a_change_applies_at_once_and_saves_after_the_debounce(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = AppPaths::in_dir(dir.path()).settings;
    let s = installed(cx, Some(&path));
    change(cx, &s, |v| v.ui_font.size = 15);
    assert_eq!(cx.update(|cx| Theme::global(cx).font_size), px(15.), "live");
    assert!(!path.exists(), "not before the debounce");
    change(cx, &s, |v| v.density = Density::Compact);
    wait_save(cx);
    let v = on_disk(&path);
    assert_eq!(
        (v["ui_font"]["size"].as_u64(), v["density"].as_str()),
        (Some(15), Some("compact"))
    );
}

#[gpui_kit::test]
fn a_fresh_start_writes_nothing(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = AppPaths::in_dir(dir.path()).settings;
    installed(cx, Some(&path));
    cx.executor().advance_clock(Duration::from_secs(2));
    cx.run_until_parked();
    assert!(!path.exists());
}

#[gpui_kit::test]
fn flush_writes_a_pending_change_at_once(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = AppPaths::in_dir(dir.path()).settings;
    let s = installed(cx, Some(&path));
    change(cx, &s, |v| v.grid_font.size = 16);
    s.update(cx, |s, cx| s.flush(cx));
    assert_eq!(
        on_disk(&path)["grid_font"]["size"].as_u64(),
        Some(16),
        "before the debounce"
    );
    wait_save(cx);
    assert_eq!(
        on_disk(&path)["grid_font"]["size"].as_u64(),
        Some(16),
        "the cancelled save writes nothing else"
    );
}

#[gpui_kit::test]
fn a_failed_flush_is_reported(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = AppPaths::in_dir(dir.path()).settings;
    let s = installed(cx, Some(&path));
    let failures = count_failures(cx, &s);
    std::fs::create_dir(&path).unwrap();
    change(cx, &s, |v| v.grid_font.size = 16);
    s.update(cx, |s, cx| s.flush(cx));
    cx.run_until_parked();
    assert_eq!(failures.get(), 1);
}

#[gpui_kit::test]
fn an_unreadable_file_is_never_overwritten(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = AppPaths::in_dir(dir.path()).settings;
    std::fs::create_dir(&path).unwrap();
    let s = installed(cx, Some(&path));
    assert_eq!(
        s.read_with(cx, |s, _| s.not_saved()),
        Some("not saved: settings.json could not be read")
    );
    change(cx, &s, |v| v.ui_font.size = 14);
    wait_save(cx);
    s.update(cx, |s, cx| s.flush(cx));
    assert!(path.is_dir());
    assert_eq!(
        cx.update(|cx| Theme::global(cx).font_size),
        px(14.),
        "still applied in memory"
    );
}

#[gpui_kit::test]
fn without_a_path_settings_live_in_memory(cx: &mut TestAppContext) {
    support::init(cx);
    let s = cx.update(|cx| settings_store::install(None, Appearance::Dark, cx));
    assert_eq!(
        s.read_with(cx, |s, _| (s.get().appearance, s.not_saved())),
        (Appearance::Dark, Some("not saved: no config folder"))
    );
}

#[gpui_kit::test]
fn a_failed_write_is_reported_once_and_the_change_stays(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = AppPaths::in_dir(dir.path()).settings;
    let s = installed(cx, Some(&path));
    std::fs::create_dir(&path).unwrap();
    let failures = count_failures(cx, &s);
    for size in [14, 15] {
        change(cx, &s, |v| v.ui_font.size = size);
        wait_save(cx);
    }
    assert_eq!(failures.get(), 1);
    assert_eq!(s.read_with(cx, |s, _| s.get().ui_font.size), 15);
}

#[gpui_kit::test]
fn reset_restores_defaults_and_keeps_unknown_fields(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let path = AppPaths::in_dir(dir.path()).settings;
    std::fs::write(
        &path,
        r#"{"future":7,"density":"compact","ui_font":{"size":15}}"#,
    )
    .unwrap();
    let s = installed(cx, Some(&path));
    s.update(cx, |s, cx| s.reset(cx));
    cx.run_until_parked();
    let got = s.read_with(cx, |s, _| s.get().clone());
    let d = Settings::default();
    assert_eq!(
        (
            got.appearance,
            &got.ui_font,
            &got.editor_font,
            &got.grid_font,
            got.density
        ),
        (
            d.appearance,
            &d.ui_font,
            &d.editor_font,
            &d.grid_font,
            d.density
        )
    );
    assert_eq!(got.extra().get("future"), Some(&serde_json::json!(7)));
    wait_save(cx);
    assert_eq!(on_disk(&path)["future"], serde_json::json!(7));
}

#[gpui_kit::test]
fn install_is_once_per_app_and_ensure_reuses_it(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let a = installed(cx, Some(&AppPaths::in_dir(dir.path()).settings));
    let b = cx.update(|cx| settings_store::install(None, Appearance::Dark, cx));
    let c = cx.update(|cx| settings_store::ensure(Appearance::Light, cx));
    assert_eq!(
        (a.entity_id(), a.entity_id()),
        (b.entity_id(), c.entity_id())
    );
    assert_eq!(
        a.read_with(cx, |s, _| s.get().appearance),
        Appearance::System
    );
}

#[gpui_kit::test]
fn ensure_without_a_store_starts_one_in_memory(cx: &mut TestAppContext) {
    support::init(cx);
    let s = cx.update(|cx| settings_store::ensure(Appearance::Dark, cx));
    assert_eq!(s.read_with(cx, |s, _| s.get().appearance), Appearance::Dark);
    assert_eq!(
        cx.update(|cx| settings_store::store(cx))
            .map(|e| e.entity_id()),
        Some(s.entity_id())
    );
}
