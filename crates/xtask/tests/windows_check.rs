//! `cargo xtask check-windows`: when it runs, what it checks, and how a skip exits.
use std::ffi::OsString;
use std::process::Command;

use xtask::windows_check::{
    CROSS_CC, Plan, TARGET, cargo_args, find_on_path, plan, rustc_program, target_installed,
};

#[test]
fn plan_runs_only_with_compiler_and_target() {
    let cc = std::path::Path::new("/usr/bin/x86_64-w64-mingw32-gcc");
    assert_eq!(plan(Some(cc), true), Plan::Run);

    let Plan::Skip(no_cc) = plan(None, true) else {
        panic!("ran without a compiler")
    };
    assert!(
        no_cc.starts_with("skipped Windows cross-check: "),
        "{no_cc}"
    );
    assert!(
        no_cc.contains(CROSS_CC) && no_cc.contains("mingw-w64-gcc"),
        "{no_cc}"
    );
    assert!(no_cc.contains("gcc-mingw-w64-x86-64"), "{no_cc}");
    assert!(!no_cc.contains("rustup"), "{no_cc}");

    let Plan::Skip(no_target) = plan(Some(cc), false) else {
        panic!("ran without the target")
    };
    assert!(
        no_target.contains(&format!("rustup target add {TARGET}")),
        "{no_target}"
    );

    let Plan::Skip(both) = plan(None, false) else {
        panic!("ran with nothing")
    };
    assert!(
        both.contains("mingw-w64-gcc") && both.contains("rustup target add"),
        "{both}"
    );
}

#[test]
fn cargo_args_check_the_three_headless_crates_without_duckdb() {
    assert_eq!(
        cargo_args(),
        [
            "check",
            "--target",
            "x86_64-pc-windows-gnu",
            "-p",
            "dbento-core",
            "-p",
            "dbento-cli",
            "-p",
            "dbento-mcp",
            "--no-default-features",
            "--features",
            "dbento-core/keychain"
        ]
    );
}

#[cfg(unix)]
#[test]
fn find_on_path_wants_an_executable_file() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let tool = dir.path().join(CROSS_CC);
    std::fs::write(&tool, "#!/bin/sh\n").unwrap();
    let path = std::env::join_paths([dir.path()]).unwrap();

    std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(find_on_path(CROSS_CC, &path), None);

    std::fs::set_permissions(&tool, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(find_on_path(CROSS_CC, &path), Some(tool));
    assert_eq!(find_on_path(CROSS_CC, &OsString::new()), None);
}

#[test]
fn target_installed_looks_in_the_sysroot() {
    let sysroot = tempfile::tempdir().unwrap();
    assert!(!target_installed(sysroot.path()));
    std::fs::create_dir_all(sysroot.path().join("lib/rustlib").join(TARGET)).unwrap();
    assert!(target_installed(sysroot.path()));
}

#[test]
fn skip_succeeds_unless_required() {
    // An empty PATH hides both the cross compiler and rustc, so the command must skip.
    let empty = tempfile::tempdir().unwrap();
    let run = |extra: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_xtask"))
            .arg("check-windows")
            .args(extra)
            .env("PATH", empty.path())
            .output()
            .unwrap()
    };
    let skipped = run(&[]);
    assert!(skipped.status.success(), "{skipped:?}");
    assert!(String::from_utf8_lossy(&skipped.stderr).contains("skipped Windows cross-check"));

    let required = run(&["--require"]);
    assert_eq!(required.status.code(), Some(1), "{required:?}");
}

#[test]
fn the_sysroot_comes_from_the_rustc_cargo_would_use() {
    let set =
        |value: &'static str| move |name: &str| (name == "RUSTC").then(|| OsString::from(value));
    assert_eq!(
        rustc_program(set("/opt/rust/bin/rustc")),
        "/opt/rust/bin/rustc"
    );
    assert_eq!(rustc_program(set("")), "rustc");
    assert_eq!(rustc_program(|_| None), "rustc");
}
