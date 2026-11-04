//! Launch arguments: at most one absolute workspace directory.

use dbento_pilot::workspace_arg;

#[test]
fn no_args_open_nothing() {
    assert_eq!(workspace_arg(&[]).unwrap(), None);
}

#[test]
fn an_absolute_dir_is_the_workspace() {
    let d = tempfile::tempdir().unwrap();
    assert_eq!(
        workspace_arg(&[d.path().to_str().unwrap().to_owned()]).unwrap(),
        Some(d.path().to_path_buf())
    );
}

#[test]
fn a_relative_path_is_refused() {
    let e = workspace_arg(&["ws".into()]).unwrap_err();
    assert!(e.to_string().contains("must be absolute"), "{e}");
}

#[test]
fn a_missing_dir_is_refused() {
    let e = workspace_arg(&["/nonexistent/dbento-ws".into()]).unwrap_err();
    assert!(e.to_string().contains("is not a directory"), "{e}");
}

#[test]
fn two_paths_are_refused() {
    let e = workspace_arg(&["/a".into(), "/b".into()]).unwrap_err();
    assert!(e.to_string().contains("at most one workspace"), "{e}");
}
