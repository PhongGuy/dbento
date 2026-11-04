use std::path::Path;

use dbento_core::paths::{STATE_DIR_VAR, StatePaths, approvals_dir, history_file};

#[test]
fn in_dir_puts_every_file_under_the_dir() {
    let p = StatePaths::in_dir(Path::new("/x"));
    assert_eq!(p.state, Path::new("/x/state.json"));
    assert_eq!(p.history, Path::new("/x/history.jsonl"));
    assert_eq!(p.drafts, Path::new("/x/drafts"));
    assert_eq!(p.approvals, Path::new("/x/approvals"));
}

#[test]
fn platform_paths_live_in_a_dbento_folder() {
    if let Some(p) = StatePaths::platform() {
        assert!(p.state.ends_with("dbento/state.json"));
        assert!(p.history.ends_with("dbento/history.jsonl"));
        assert!(p.drafts.ends_with("dbento/drafts"));
        assert!(p.approvals.ends_with("dbento/approvals"));
        assert_eq!(p.history.parent(), p.approvals.parent());
    }
}

#[test]
fn a_set_state_dir_wins_and_an_empty_one_is_ignored() {
    let dir = tempfile::tempdir().unwrap();
    assert_eq!(
        StatePaths::for_state_dir(Some(dir.path().into())),
        Some(StatePaths::in_dir(dir.path()))
    );
    assert_eq!(
        StatePaths::for_state_dir(Some("".into())),
        StatePaths::platform()
    );
    assert_eq!(StatePaths::for_state_dir(None), StatePaths::platform());
}

#[test]
fn the_file_helpers_read_the_state_dir_variable() {
    assert_eq!(STATE_DIR_VAR, "DBENTO_STATE_DIR");
    let expected = StatePaths::for_state_dir(std::env::var_os(STATE_DIR_VAR));
    assert_eq!(history_file(), expected.as_ref().map(|p| p.history.clone()));
    assert_eq!(approvals_dir(), expected.map(|p| p.approvals));
}

#[test]
fn settings_and_keymap_sit_beside_the_state_file() {
    let p = StatePaths::in_dir(Path::new("/x"));
    assert_eq!(p.settings, Path::new("/x/settings.json"));
    assert_eq!(p.keymap, Path::new("/x/keymap.json"));
    if let Some(p) = StatePaths::platform() {
        assert!(p.settings.ends_with("dbento/settings.json"));
        assert!(p.keymap.ends_with("dbento/keymap.json"));
        assert_eq!(p.settings.parent(), p.state.parent());
        assert_eq!(p.keymap.parent(), p.state.parent());
    }
}

#[test]
fn the_state_dir_moves_settings_and_keymap_too() {
    let dir = tempfile::tempdir().unwrap();
    let p = StatePaths::for_state_dir(Some(dir.path().into())).unwrap();
    assert_eq!(p.settings, dir.path().join("settings.json"));
    assert_eq!(p.keymap, dir.path().join("keymap.json"));
}
