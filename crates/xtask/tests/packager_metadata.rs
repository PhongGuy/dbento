//! The committed packager metadata of dbento-app, read through the real `cargo metadata`.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};

const PNG_SIZES: [u32; 9] = [16, 24, 32, 48, 64, 128, 256, 512, 1024];

/// The raw `[package.metadata.packager]` section and dbento-app's manifest directory.
fn section() -> (Value, PathBuf) {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let out = Command::new(cargo)
        .args(["metadata", "--no-deps", "--format-version", "1"])
        .current_dir(xtask::workspace_root())
        .output()
        .unwrap();
    assert!(out.status.success(), "{out:?}");
    let metadata: Value = serde_json::from_slice(&out.stdout).unwrap();
    let package = metadata["packages"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["name"] == "dbento-app")
        .expect("dbento-app is a workspace member")
        .clone();
    let dir = Path::new(package["manifest_path"].as_str().unwrap())
        .parent()
        .unwrap()
        .to_path_buf();
    (package["metadata"]["packager"].clone(), dir)
}

#[test]
fn identity_and_binaries() {
    let (c, _) = section();
    assert_eq!(c["productName"], "Dbento");
    assert_eq!(c["identifier"], "dev.dbento.app");
    assert_eq!(c["category"], "DeveloperTool");
    assert_eq!(
        c["binaries"],
        json!([{ "path": "dbento-app", "main": true }, { "path": "dbento", "main": false }])
    );
    assert!(
        c.get("formats").is_none(),
        "formats are chosen per OS on the command line"
    );
}

#[test]
fn nothing_secret_or_derived_is_committed() {
    let (c, _) = section();
    for key in ["name", "version", "binariesDir", "outDir"] {
        assert!(
            c.get(key).is_none(),
            "{key} is filled by cargo xtask packager-config"
        );
    }
    assert!(
        c.get("macos").is_none() && c.get("windows").is_none(),
        "signing comes from the environment"
    );
}

#[test]
fn deb_declares_gpui_libraries() {
    let (c, _) = section();
    assert_eq!(
        c["deb"]["depends"],
        json!([
            "libxcb1",
            "libxkbcommon0",
            "libxkbcommon-x11-0",
            "libvulkan1",
            "libwayland-client0",
            "libwayland-egl1",
            "libegl1"
        ])
    );
    assert_eq!(c["deb"]["section"], "devel");
}

#[test]
fn icons_point_at_committed_exports() {
    let (c, dir) = section();
    let icons: Vec<PathBuf> = c["icons"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| dir.join(i.as_str().unwrap()))
        .collect();
    assert_eq!(icons.len(), 3);
    assert_eq!(icons[0].file_name().unwrap(), "*.png");
    let png_dir = icons[0].parent().unwrap();
    for size in PNG_SIZES {
        assert!(
            png_dir.join(format!("{size}x{size}.png")).is_file(),
            "{size}"
        );
    }
    assert!(icons[1].ends_with("dbento.icns") && icons[1].is_file());
    assert!(icons[2].ends_with("dbento.ico") && icons[2].is_file());
}

#[test]
fn the_desktop_template_names_the_window_class() {
    let (c, dir) = section();
    assert_eq!(c["deb"]["desktopTemplate"], "packaging/dbento-app.desktop");
    let path = dir.join("packaging/dbento-app.desktop");
    let text = std::fs::read_to_string(&path).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    for want in [
        "[Desktop Entry]",
        "Exec={{exec}} {{exec_arg}}",
        "Icon={{icon}}",
        "Name={{name}}",
        "StartupWMClass=dbento-app",
        "Type=Application",
    ] {
        assert!(
            lines.contains(&want),
            "{want} missing from {}",
            path.display()
        );
    }
}

#[test]
fn resources_ship_every_licence_text() {
    let (c, dir) = section();
    assert_eq!(c["resources"], json!(["../../licenses/*.txt"]));
    let mut names: Vec<String> = std::fs::read_dir(dir.join("../../licenses"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.ends_with(".txt"))
        .collect();
    names.sort();
    assert_eq!(
        names,
        ["Inter-OFL.txt", "JetBrainsMono-OFL.txt", "Lucide-ISC.txt"]
    );
}
