use dbento_app::app::AppPaths;

#[test]
fn a_state_dir_puts_every_app_file_inside_it() {
    let dir = tempfile::tempdir().unwrap();
    let paths = AppPaths::for_state_dir(Some(dir.path().into())).unwrap();
    assert_eq!(paths, AppPaths::in_dir(dir.path()));
}

#[test]
fn an_unset_or_empty_state_dir_keeps_the_platform_paths() {
    assert_eq!(AppPaths::for_state_dir(None), AppPaths::platform());
    assert_eq!(
        AppPaths::for_state_dir(Some("".into())),
        AppPaths::platform()
    );
}
