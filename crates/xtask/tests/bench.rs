#[test]
fn the_bench_runs_the_release_binary_with_the_given_flags() {
    let extra = vec!["--only".to_owned(), "ttfr_pg".to_owned()];
    assert_eq!(
        xtask::bench::cargo_args(&extra),
        [
            "run",
            "--release",
            "--locked",
            "--quiet",
            "-p",
            "dbento-bench",
            "--",
            "run",
            "--only",
            "ttfr_pg"
        ]
    );
}

#[test]
fn a_bench_error_exits_2_never_1() {
    // A cargo that cannot be spawned makes `bench::run` fail; 1 would read as "target missed".
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["bench", "--only", "ttfr_pg"])
        .env("CARGO", "/nonexistent/cargo")
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(2),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert!(String::from_utf8_lossy(&out.stderr).contains("error"));
}

#[test]
#[cfg(unix)]
fn a_failed_release_build_exits_2_not_cargos_101() {
    // A stand-in cargo that fails every command the way a broken build does.
    use std::os::unix::fs::PermissionsExt as _;
    let dir = tempfile::tempdir().unwrap();
    let cargo = dir.path().join("cargo");
    std::fs::write(&cargo, "#!/bin/sh\nexit 101\n").unwrap();
    std::fs::set_permissions(&cargo, std::fs::Permissions::from_mode(0o755)).unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["bench", "--only", "ttfr_pg"])
        .env("CARGO", &cargo)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("build"), "{stderr}");
}
