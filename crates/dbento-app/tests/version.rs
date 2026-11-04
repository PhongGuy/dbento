#[test]
fn version_prints_without_a_display() {
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_dbento-app"))
        .arg("--version")
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert_eq!(
        String::from_utf8(out.stdout).unwrap().trim(),
        format!("dbento-app {}", env!("CARGO_PKG_VERSION"))
    );
}
