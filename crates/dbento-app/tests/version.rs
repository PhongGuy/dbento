use std::process::{Command, Output, Stdio};
use std::time::{Duration, Instant};

/// Runs the app with no display; a run still going after the deadline is killed and fails,
/// so a regression to starting GPUI shows as a failure instead of a hung suite.
fn run_headless(args: &[&std::ffi::OsStr]) -> Output {
    let mut child = Command::new(env!("CARGO_BIN_EXE_dbento-app"))
        .args(args)
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while child.try_wait().unwrap().is_none() {
        if Instant::now() > deadline {
            child.kill().unwrap();
            panic!("dbento-app {args:?} did not exit");
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    child.wait_with_output().unwrap()
}

#[test]
fn version_prints_without_a_display() {
    let out = run_headless(&["--version".as_ref()]);
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8(out.stdout).unwrap().trim(),
        format!("dbento-app {}", env!("CARGO_PKG_VERSION"))
    );
}

#[test]
fn help_prints_usage_without_a_display() {
    for flag in ["--help", "-h"] {
        let out = run_headless(&[flag.as_ref()]);
        assert_eq!(out.status.code(), Some(0), "{flag}");
        let text = String::from_utf8(out.stdout).unwrap();
        assert!(text.contains("Usage: dbento-app [WORKSPACE]"), "{text}");
    }
}

#[test]
fn an_unknown_flag_prints_usage_and_exits_2() {
    let out = run_headless(&["--bogus".as_ref()]);
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(
        err.contains("--bogus") && err.contains("Usage: dbento-app"),
        "{err}"
    );
}

#[test]
fn a_missing_workspace_exits_2_naming_it() {
    let d = tempfile::tempdir().unwrap();
    let missing = d.path().join("nope");
    let out = run_headless(&[missing.as_os_str()]);
    assert_eq!(out.status.code(), Some(2));
    let err = String::from_utf8(out.stderr).unwrap();
    assert!(err.contains(&missing.display().to_string()), "{err}");
}

#[cfg(target_os = "linux")]
#[test]
fn no_display_exits_1_with_a_message() {
    let d = tempfile::tempdir().unwrap();
    let out = run_headless(&[d.path().as_os_str()]);
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(
        String::from_utf8(out.stderr).unwrap().trim(),
        "dbento-app needs a graphical display (WAYLAND_DISPLAY or DISPLAY is not set)"
    );
}
