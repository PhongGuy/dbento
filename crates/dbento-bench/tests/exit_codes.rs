use std::process::Command;

fn run(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_dbento-bench"))
        .args(args)
        .env_remove("DBENTO_PG_17_STRESS_URL")
        .env_remove("DBENTO_STRESS_SQLITE")
        .output()
        .unwrap()
}

#[test]
fn a_setup_error_exits_2() {
    assert_eq!(run(&["run", "--only", "nosuch"]).status.code(), Some(2));
    assert_eq!(run(&["run", "--only", "ttfr_pg"]).status.code(), Some(2));
}

#[test]
fn a_bad_url_error_never_shows_the_password() {
    let out = run(&[
        "run",
        "--only",
        "ttfr_pg",
        "--pg-url",
        "postgres://u:hunter2secret@h/db",
    ]);
    assert_eq!(out.status.code(), Some(2));
    assert!(!String::from_utf8_lossy(&out.stderr).contains("hunter2secret"));
}
