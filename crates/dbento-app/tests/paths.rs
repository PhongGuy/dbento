use dbento_app::app::AppPaths;
use std::path::Path;
#[test]
fn in_dir_puts_every_file_under_the_dir() {
    let p = AppPaths::in_dir(Path::new("/x"));
    assert_eq!(p.state, Path::new("/x/state.json"));
    assert_eq!(p.history, Path::new("/x/history.jsonl"));
    assert_eq!(p.drafts, Path::new("/x/drafts"));
    assert_eq!(p.approvals, Path::new("/x/approvals"));
}
#[test]
fn platform_paths_live_in_a_dbento_folder() {
    if let Some(p) = AppPaths::platform() {
        assert!(p.state.ends_with("dbento/state.json"));
        assert!(p.history.ends_with("dbento/history.jsonl"));
        assert!(p.drafts.ends_with("dbento/drafts"));
        assert!(p.approvals.ends_with("dbento/approvals"));
    }
}

#[test]
fn the_app_paths_are_the_core_state_paths() {
    use dbento_core::paths::StatePaths;
    let core = StatePaths::in_dir(Path::new("/x"));
    let app = AppPaths::in_dir(Path::new("/x"));
    assert_eq!(
        (app.state, app.history, app.drafts, app.approvals),
        (core.state, core.history, core.drafts, core.approvals)
    );
    assert_eq!(
        AppPaths::platform().map(|p| p.history),
        StatePaths::platform().map(|p| p.history)
    );
}

#[test]
fn the_app_paths_carry_settings_and_keymap() {
    use dbento_core::paths::StatePaths;
    let core = StatePaths::in_dir(Path::new("/x"));
    let app = AppPaths::in_dir(Path::new("/x"));
    assert_eq!((app.settings, app.keymap), (core.settings, core.keymap));
}
