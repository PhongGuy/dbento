//! The app's command line: flags, the optional workspace folder, and the display check.

use std::ffi::OsString;

use dbento_app::launch::{Launch, LaunchError, has_display, parse};

fn args(list: &[&str]) -> Vec<OsString> {
    list.iter().map(OsString::from).collect()
}

#[test]
fn no_arguments_open_without_a_workspace() {
    assert_eq!(parse(args(&[])).unwrap(), Launch::Open(None));
}

#[test]
fn help_flags_ask_for_usage() {
    assert_eq!(parse(args(&["--help"])).unwrap(), Launch::Help);
    assert_eq!(parse(args(&["-h"])).unwrap(), Launch::Help);
}

#[test]
fn version_flags_ask_for_the_version() {
    assert_eq!(parse(args(&["--version"])).unwrap(), Launch::Version);
    assert_eq!(parse(args(&["-V"])).unwrap(), Launch::Version);
}

#[test]
fn an_unknown_flag_is_refused() {
    assert_eq!(
        parse(args(&["--bogus"])).unwrap_err(),
        LaunchError::UnknownFlag("--bogus".into())
    );
}

#[test]
fn an_existing_folder_opens_as_an_absolute_workspace() {
    let d = tempfile::tempdir().unwrap();
    let got = parse(vec![d.path().as_os_str().to_owned()]).unwrap();
    assert_eq!(got, Launch::Open(Some(d.path().to_path_buf())));
}

#[test]
fn a_missing_folder_is_refused_by_name() {
    let d = tempfile::tempdir().unwrap();
    let missing = d.path().join("nope");
    let err = parse(vec![missing.clone().into_os_string()]).unwrap_err();
    assert_eq!(err, LaunchError::NotFound(missing.clone()));
    assert!(
        err.to_string().contains(&missing.display().to_string()),
        "{err}"
    );
}

#[test]
fn a_file_is_refused_as_a_workspace() {
    let f = tempfile::NamedTempFile::new().unwrap();
    let err = parse(vec![f.path().as_os_str().to_owned()]).unwrap_err();
    assert_eq!(err, LaunchError::NotADirectory(f.path().to_path_buf()));
}

#[test]
fn a_second_folder_is_refused() {
    let d = tempfile::tempdir().unwrap();
    let p = d.path().as_os_str().to_owned();
    let err = parse(vec![p.clone(), p]).unwrap_err();
    assert!(matches!(err, LaunchError::ExtraArgument(_)), "{err:?}");
}

#[test]
fn a_display_needs_wayland_or_x11() {
    let none = |_: &str| None::<OsString>;
    assert!(!has_display(none));
    assert!(!has_display(|_: &str| Some(OsString::new())));
    assert!(has_display(
        |v: &str| (v == "WAYLAND_DISPLAY").then(|| "wayland-0".into())
    ));
    assert!(has_display(|v: &str| (v == "DISPLAY").then(|| ":0".into())));
}
