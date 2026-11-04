#![cfg(target_os = "linux")]

use std::path::Path;
use std::process::Command;

/// Runs `scripts/install.sh <action>` from `tmp` with cargo skipped, the data in `tmp/data`
/// and `prefix` (relative to `tmp`) as the install root.
fn script_in(action: &str, tmp: &Path, prefix: Option<&str>) -> std::process::Output {
    let mut cmd = Command::new("bash");
    cmd.arg(xtask::workspace_root().join("scripts/install.sh"))
        .arg(action)
        .current_dir(tmp)
        .env_remove("DBENTO_PREFIX")
        .env("DBENTO_INSTALL_SKIP_CARGO", "1")
        .env("XDG_DATA_HOME", tmp.join("data"));
    if let Some(prefix) = prefix {
        cmd.env("DBENTO_PREFIX", prefix);
    }
    cmd.output().unwrap()
}

fn script(action: &str, tmp: &Path) -> std::process::Output {
    script_in(action, tmp, Some("prefix"))
}

fn entry(tmp: &Path) -> String {
    std::fs::read_to_string(tmp.join("data/applications/dbento-app.desktop")).unwrap()
}

/// The hicolor sizes `make install` puts on disk.
const SIZES: [u32; 8] = [16, 24, 32, 48, 64, 128, 256, 512];

fn icon(tmp: &Path, size: u32) -> std::path::PathBuf {
    tmp.join(format!(
        "data/icons/hicolor/{size}x{size}/apps/dbento-app.png"
    ))
}

fn old_files(tmp: &Path) -> [std::path::PathBuf; 2] {
    [
        tmp.join("data/applications/dbento.desktop"),
        tmp.join("data/icons/hicolor/scalable/apps/dbento.svg"),
    ]
}

fn seed_old_files(tmp: &Path) {
    for f in old_files(tmp) {
        std::fs::create_dir_all(f.parent().unwrap()).unwrap();
        std::fs::write(&f, b"old").unwrap();
    }
}

#[test]
fn install_writes_the_desktop_entry_and_icons_and_uninstall_removes_them() {
    let tmp = tempfile::tempdir().unwrap();
    let out = script("install", tmp.path());
    assert!(out.status.success(), "{out:?}");

    let desktop = tmp.path().join("data/applications/dbento-app.desktop");
    let entry = std::fs::read_to_string(&desktop).unwrap();
    let exec = format!(
        "Exec=\"{}/prefix/bin/dbento-app\" %f",
        tmp.path().canonicalize().unwrap().display()
    );
    for line in [
        "[Desktop Entry]",
        "Name=Dbento",
        exec.as_str(),
        "Icon=dbento-app",
        "StartupWMClass=dbento-app",
        "Type=Application",
        "Categories=Development;Database;",
        "Terminal=false",
    ] {
        assert!(entry.lines().any(|l| l == line), "{line} missing:\n{entry}");
    }
    for size in SIZES {
        let committed =
            xtask::workspace_root().join(format!("assets/icon/generated/png/{size}x{size}.png"));
        assert_eq!(
            std::fs::read(icon(tmp.path(), size)).unwrap(),
            std::fs::read(committed).unwrap(),
            "{size}"
        );
    }
    assert!(
        !tmp.path().join("data/icons/hicolor/1024x1024").exists(),
        "hicolor has no 1024 directory"
    );
    // The validator is optional on the host; when present the entry must pass it.
    if let Ok(v) = Command::new("desktop-file-validate").arg(&desktop).output() {
        assert!(v.status.success() && v.stdout.is_empty(), "{v:?}");
    }

    let out = script("uninstall", tmp.path());
    assert!(out.status.success(), "{out:?}");
    assert!(!desktop.exists());
    for size in SIZES {
        assert!(!icon(tmp.path(), size).exists(), "{size}");
    }
}

#[test]
fn install_replaces_the_old_entry_and_scalable_icon() {
    let tmp = tempfile::tempdir().unwrap();
    seed_old_files(tmp.path());
    let out = script("install", tmp.path());
    assert!(out.status.success(), "{out:?}");
    for f in old_files(tmp.path()) {
        assert!(!f.exists(), "{} kept", f.display());
    }
    assert!(
        tmp.path()
            .join("data/applications/dbento-app.desktop")
            .exists()
    );
}

#[test]
fn uninstall_removes_the_old_entry_too() {
    let tmp = tempfile::tempdir().unwrap();
    seed_old_files(tmp.path());
    let out = script("uninstall", tmp.path());
    assert!(out.status.success(), "{out:?}");
    for f in old_files(tmp.path()) {
        assert!(!f.exists(), "{} kept", f.display());
    }
}

#[test]
fn an_unknown_action_is_a_usage_error() {
    let tmp = tempfile::tempdir().unwrap();
    assert_eq!(script("frobnicate", tmp.path()).status.code(), Some(2));
}

#[test]
fn a_prefix_with_a_space_is_absolute_and_quoted_in_exec() {
    let tmp = tempfile::tempdir().unwrap();
    let out = script_in("install", tmp.path(), Some("my $dir"));
    assert!(out.status.success(), "{out:?}");
    let want = format!(
        "Exec=\"{}/my \\\\$dir/bin/dbento-app\" %f",
        tmp.path().canonicalize().unwrap().display()
    );
    let entry = entry(tmp.path());
    assert!(entry.lines().any(|l| l == want), "{want} missing:\n{entry}");
    if let Ok(v) = Command::new("desktop-file-validate")
        .arg(tmp.path().join("data/applications/dbento-app.desktop"))
        .output()
    {
        assert!(v.status.success() && v.stdout.is_empty(), "{v:?}");
    }
}

#[test]
fn an_exported_prefix_does_not_move_the_install() {
    let tmp = tempfile::tempdir().unwrap();
    let out = Command::new("bash")
        .arg(xtask::workspace_root().join("scripts/install.sh"))
        .arg("install")
        .env_remove("DBENTO_PREFIX")
        .env("PREFIX", tmp.path().join("stray"))
        .env("DBENTO_INSTALL_SKIP_CARGO", "1")
        .env("XDG_DATA_HOME", tmp.path().join("data"))
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    assert!(
        !entry(tmp.path()).contains("stray"),
        "{}",
        entry(tmp.path())
    );
}
