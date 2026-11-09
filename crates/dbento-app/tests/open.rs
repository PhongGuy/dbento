use crate::support;
use dbento_app::app::{AppPaths, SavedState, initial_workspace, start};
use dbento_app::persist::{load_json, save_json};
use gpui_kit::TestAppContext;
use std::path::PathBuf;

#[gpui_kit::test]
fn an_argument_opens_that_workspace_and_is_remembered(cx: &mut TestAppContext) {
    support::init(cx);
    let (fx, files) = (support::fixture("", ""), tempfile::tempdir().unwrap());
    let (win, _) = cx
        .update(|cx| {
            start(
                support::services(files.path()),
                Some(fx.dir.path().into()),
                cx,
            )
        })
        .unwrap();
    assert!(support::present(cx, win, "connection-list"));
    assert!(!support::present(cx, win, "welcome-open"));
    let saved: SavedState = load_json(&AppPaths::in_dir(files.path()).state).value;
    assert_eq!(saved.last_workspace.as_deref(), Some(fx.dir.path()));
}
#[gpui_kit::test]
fn without_an_argument_the_last_workspace_opens(cx: &mut TestAppContext) {
    support::init(cx);
    let (fx, files) = (support::fixture("", ""), tempfile::tempdir().unwrap());
    let state = SavedState {
        last_workspace: Some(fx.dir.path().into()),
        ..SavedState::default()
    };
    save_json(&AppPaths::in_dir(files.path()).state, &state).unwrap();
    let (win, _) = cx
        .update(|cx| start(support::services(files.path()), None, cx))
        .unwrap();
    assert!(support::present(cx, win, "connection-list"));
}
#[gpui_kit::test]
fn with_nothing_to_open_the_welcome_view_shows(cx: &mut TestAppContext) {
    support::init(cx);
    let files = tempfile::tempdir().unwrap();
    let (win, _) = cx
        .update(|cx| start(support::services(files.path()), None, cx))
        .unwrap();
    assert!(support::present(cx, win, "welcome-open"));
    assert!(!support::present(cx, win, "welcome-error"));
}
#[gpui_kit::test]
fn a_folder_without_dbento_toml_shows_the_error(cx: &mut TestAppContext) {
    support::init(cx);
    let (empty, files) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (win, shell) = cx
        .update(|cx| {
            start(
                support::services(files.path()),
                Some(empty.path().into()),
                cx,
            )
        })
        .unwrap();
    let error = support::label_of(cx, win, "welcome-error").expect("an error is shown");
    assert!(error.contains("no dbento.toml"), "{error}");
    assert!(shell.read_with(cx, |s, _| s.workspace().is_none()));
    assert!(!AppPaths::in_dir(files.path()).state.exists());
}
#[test]
fn the_argument_wins_over_the_saved_workspace() {
    let saved = SavedState {
        last_workspace: Some(PathBuf::from("/b")),
        ..SavedState::default()
    };
    assert_eq!(
        initial_workspace(Some("/a".into()), &saved),
        Some(PathBuf::from("/a"))
    );
    assert_eq!(initial_workspace(None, &saved), Some(PathBuf::from("/b")));
    assert_eq!(initial_workspace(None, &SavedState::default()), None);
}
#[gpui_kit::test]
// Otherwise the same error greets every later start until another folder opens.
fn a_saved_workspace_that_fails_to_open_is_forgotten(cx: &mut TestAppContext) {
    support::init(cx);
    let (empty, files) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let paths = AppPaths::in_dir(files.path());
    let state = SavedState {
        last_workspace: Some(empty.path().into()),
        ..SavedState::default()
    };
    save_json(&paths.state, &state).unwrap();
    let (win, _) = cx
        .update(|cx| start(support::services(files.path()), None, cx))
        .unwrap();
    assert!(support::present(cx, win, "welcome-error"));
    let saved: SavedState = load_json(&paths.state).value;
    assert_eq!(saved.last_workspace, None);
}
#[gpui_kit::test]
fn a_bad_argument_keeps_the_saved_workspace(cx: &mut TestAppContext) {
    support::init(cx);
    let (empty, files, fx) = (
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        support::fixture("", ""),
    );
    let paths = AppPaths::in_dir(files.path());
    let state = SavedState {
        last_workspace: Some(fx.dir.path().into()),
        ..SavedState::default()
    };
    save_json(&paths.state, &state).unwrap();
    let (win, _) = cx
        .update(|cx| {
            start(
                support::services(files.path()),
                Some(empty.path().into()),
                cx,
            )
        })
        .unwrap();
    assert!(support::present(cx, win, "welcome-error"));
    let saved: SavedState = load_json(&paths.state).value;
    assert_eq!(saved, state);
}
