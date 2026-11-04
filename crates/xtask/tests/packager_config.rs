//! `cargo xtask packager-config`: the generated cargo-packager config.

use std::path::Path;
use std::process::Command;

use serde_json::{Value, json};
use xtask::packager_config::{Signing, packager_config, write_private};

const NOTICES: &str = "/n/THIRD-PARTY-NOTICES.html";

fn metadata(packager: Value) -> Value {
    json!({
        "target_directory": "/t",
        "packages": [
            { "name": "dbento-core", "version": "9.9.9", "manifest_path": "/r/crates/dbento-core/Cargo.toml", "metadata": null },
            {
                "name": "dbento-app",
                "version": "1.2.3",
                "manifest_path": "/r/crates/dbento-app/Cargo.toml",
                "metadata": { "packager": packager }
            }
        ]
    })
}

fn base() -> Value {
    json!({
        "productName": "Dbento",
        "icons": ["../../assets/icon/generated/png/*.png", "/abs/dbento.icns"],
        "macos": { "minimumSystemVersion": "11.0" },
        "deb": { "depends": ["libxcb1"] }
    })
}

fn config(m: &Value, target: Option<&str>, signing: &Signing) -> anyhow::Result<Value> {
    packager_config(m, target, Path::new("/o"), Path::new(NOTICES), signing)
}

fn unsigned(m: &Value, target: Option<&str>) -> Value {
    config(m, target, &Signing::default()).unwrap()
}

#[test]
fn fills_name_version_and_directories() {
    let c = unsigned(&metadata(base()), None);
    assert_eq!(c["name"], "dbento-app");
    assert_eq!(c["version"], "1.2.3");
    assert_eq!(c["binariesDir"], "/t/release");
    assert_eq!(c["outDir"], "/o");
    assert_eq!(c["productName"], "Dbento");
    assert_eq!(c["deb"]["depends"], json!(["libxcb1"]));
    let cross = unsigned(&metadata(base()), Some("aarch64-unknown-linux-gnu"));
    assert_eq!(cross["binariesDir"], "/t/aarch64-unknown-linux-gnu/release");
}

#[test]
fn icons_resolve_against_the_manifest_directory() {
    assert_eq!(
        unsigned(&metadata(base()), None)["icons"],
        json!([
            "/r/crates/dbento-app/../../assets/icon/generated/png/*.png",
            "/abs/dbento.icns"
        ])
    );
}

#[test]
fn signing_is_added_only_from_the_environment() {
    let plain = unsigned(&metadata(base()), None);
    assert_eq!(plain["macos"], json!({ "minimumSystemVersion": "11.0" }));
    assert!(plain.get("windows").is_none());

    let signing = Signing {
        macos_identity: Some("Developer ID Application: Example (TEAM123)".into()),
        windows_sign_command: Some("signtool sign %1".into()),
    };
    let signed = config(&metadata(base()), None, &signing).unwrap();
    assert_eq!(
        signed["macos"]["signingIdentity"],
        "Developer ID Application: Example (TEAM123)"
    );
    assert_eq!(signed["macos"]["minimumSystemVersion"], "11.0");
    assert_eq!(signed["windows"]["signCommand"], "signtool sign %1");
}

#[test]
fn blank_variables_mean_unsigned() {
    let vars = |name: &str| match name {
        "DBENTO_MACOS_SIGNING_IDENTITY" => Some("  ".to_owned()),
        "DBENTO_WINDOWS_SIGN_COMMAND" => Some(String::new()),
        _ => None,
    };
    assert_eq!(Signing::from_vars(vars), Signing::default());
    let set =
        Signing::from_vars(|n| (n == "DBENTO_MACOS_SIGNING_IDENTITY").then(|| "Dev".to_owned()));
    assert_eq!(set.macos_identity.as_deref(), Some("Dev"));
    assert_eq!(set.windows_sign_command, None);
    let apinata = Signing::from_vars(|n| n.starts_with("APINATA_").then(|| "x".to_owned()));
    assert_eq!(
        apinata,
        Signing::default(),
        "only DBENTO_ variables are read"
    );
}

#[test]
fn committed_signing_settings_are_refused() {
    for (section, key) in [
        ("macos", "signingIdentity"),
        ("macos", "signing-identity"),
        ("macos", "signing_identity"),
        ("windows", "signCommand"),
        ("windows", "sign-command"),
        ("windows", "sign_command"),
        ("windows", "certificateThumbprint"),
        ("windows", "certificate-thumbprint"),
        ("windows", "certificate_thumbprint"),
    ] {
        let mut committed = base();
        committed[section] = json!({ key: "x" });
        let err = config(&metadata(committed), None, &Signing::default()).unwrap_err();
        assert!(err.to_string().contains(key), "{err}");
    }
}

#[test]
fn a_missing_package_or_section_is_an_error() {
    let no_app = json!({ "target_directory": "/t", "packages": [] });
    assert!(config(&no_app, None, &Signing::default()).is_err());
    let mut no_section = metadata(base());
    no_section["packages"][1]["metadata"] = Value::Null;
    let err = config(&no_section, None, &Signing::default()).unwrap_err();
    assert!(
        err.to_string().contains("[package.metadata.packager]"),
        "{err}"
    );
}

#[cfg(unix)]
#[test]
fn write_private_leaves_an_owner_only_file() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("packager-config.json");
    std::fs::write(&path, "old").unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();
    write_private(&path, b"{}").unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"{}");
    assert_eq!(
        std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
}

#[test]
fn the_desktop_template_resolves_against_the_manifest_directory() {
    let mut p = base();
    p["deb"]["desktopTemplate"] = json!("packaging/dbento-app.desktop");
    let c = unsigned(&metadata(p), None);
    assert_eq!(
        c["deb"]["desktopTemplate"],
        "/r/crates/dbento-app/packaging/dbento-app.desktop"
    );
    assert_eq!(c["deb"]["depends"], json!(["libxcb1"]));
}

#[test]
fn a_config_without_a_template_gains_none() {
    assert!(
        unsigned(&metadata(base()), None)["deb"]
            .get("desktopTemplate")
            .is_none()
    );
}

#[test]
fn resources_resolve_and_end_with_the_notices() {
    let mut p = base();
    p["resources"] = json!([
        "../../licenses/*.txt",
        { "src": "notes/README.txt", "target": "doc/README.txt" },
        "/abs/x.txt"
    ]);
    assert_eq!(
        unsigned(&metadata(p), None)["resources"],
        json!([
            "/r/crates/dbento-app/../../licenses/*.txt",
            { "src": "/r/crates/dbento-app/notes/README.txt", "target": "doc/README.txt" },
            "/abs/x.txt",
            NOTICES
        ])
    );
}

#[test]
fn a_config_without_resources_gains_only_the_notices() {
    assert_eq!(
        unsigned(&metadata(base()), None)["resources"],
        json!([NOTICES])
    );
}

/// Runs the real command against the committed metadata.
fn command(dir: &Path, notices: &Path, macos_identity: Option<&str>) -> std::process::Output {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_xtask"));
    cmd.arg("packager-config")
        .arg("--out-dir")
        .arg(dir.join("dist"))
        .arg("--output")
        .arg(dir.join("cfg.json"))
        .arg("--notices")
        .arg(notices)
        .env_remove("DBENTO_WINDOWS_SIGN_COMMAND");
    match macos_identity {
        Some(id) => cmd.env("DBENTO_MACOS_SIGNING_IDENTITY", id),
        None => cmd.env_remove("DBENTO_MACOS_SIGNING_IDENTITY"),
    };
    cmd.output().unwrap()
}

#[cfg(unix)]
#[test]
fn the_command_writes_a_private_absolute_config() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let notices = dir.path().join("THIRD-PARTY-NOTICES.html");
    std::fs::write(&notices, "<html></html>").unwrap();
    let out = command(dir.path(), &notices, Some("-"));
    assert!(out.status.success(), "{out:?}");
    let output = dir.path().join("cfg.json");
    assert_eq!(
        std::fs::metadata(&output).unwrap().permissions().mode() & 0o777,
        0o600
    );
    let c: Value = serde_json::from_slice(&std::fs::read(&output).unwrap()).unwrap();
    assert_eq!(c["name"], "dbento-app");
    assert_eq!(c["identifier"], "dev.dbento.app");
    assert_eq!(
        c["version"],
        env!("CARGO_PKG_VERSION"),
        "xtask shares the workspace version"
    );
    assert_eq!(c["macos"]["signingIdentity"], "-");
    assert!(c.get("windows").is_none());
    let absolute = |v: &Value| Path::new(v.as_str().unwrap()).is_absolute();
    assert!(
        c["icons"].as_array().unwrap().iter().all(absolute),
        "{}",
        c["icons"]
    );
    assert!(
        c["resources"].as_array().unwrap().iter().all(absolute),
        "{}",
        c["resources"]
    );
    assert!(absolute(&c["deb"]["desktopTemplate"]));
    assert!(absolute(&c["binariesDir"]) && absolute(&c["outDir"]));
    assert_eq!(
        c["resources"].as_array().unwrap().last().unwrap(),
        notices.to_str().unwrap()
    );
}

#[test]
fn the_command_refuses_without_generated_notices() {
    let dir = tempfile::tempdir().unwrap();
    let out = command(dir.path(), &dir.path().join("missing.html"), None);
    assert!(!out.status.success(), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stderr).contains("cargo xtask notices"));
    assert!(!dir.path().join("cfg.json").exists());
}

#[test]
fn a_committed_resources_value_that_is_not_an_array_is_an_error() {
    let mut committed = base();
    committed["resources"] = json!("../../licenses/*.txt");
    let err = config(&metadata(committed), None, &Signing::default()).unwrap_err();
    assert!(
        err.to_string().contains("resources is not an array"),
        "{err}"
    );
}

#[test]
fn a_signing_value_never_drops_silently_on_a_non_table_section() {
    for (section, signing) in [
        (
            "macos",
            Signing {
                macos_identity: Some("Dev".into()),
                ..Signing::default()
            },
        ),
        (
            "windows",
            Signing {
                windows_sign_command: Some("signtool sign %1".into()),
                ..Signing::default()
            },
        ),
    ] {
        let mut committed = base();
        committed[section] = json!("not a table");
        let err = config(&metadata(committed), None, &signing).unwrap_err();
        assert!(err.to_string().contains(section), "{err}");
    }
}
