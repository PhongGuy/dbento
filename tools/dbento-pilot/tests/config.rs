//! The repository's pilot wiring: `gpui-pilot.toml` builds and launches this package, and
//! `.mcp.json` registers the server.

use std::path::{Path, PathBuf};

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

#[test]
fn the_headless_config_builds_and_runs_this_package() {
    let text = std::fs::read_to_string(repo_root().join("gpui-pilot.toml")).unwrap();
    let config: toml::Table = toml::from_str(&text).unwrap();
    let headless = config["headless"].as_table().unwrap();
    let build: Vec<&str> = headless["build"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(
        build,
        [
            "cargo",
            "build",
            "--manifest-path",
            "tools/dbento-pilot/Cargo.toml"
        ]
    );
    assert!(repo_root().join(build[3]).is_file());
    assert_eq!(
        headless["binary"].as_str(),
        Some("tools/dbento-pilot/target/debug/dbento-pilot")
    );
}

#[test]
fn the_session_config_builds_and_runs_the_optimised_app() {
    let text = std::fs::read_to_string(repo_root().join("gpui-pilot.toml")).unwrap();
    let config: toml::Table = toml::from_str(&text).unwrap();
    let session = config["session"].as_table().unwrap();
    let build: Vec<&str> = session["build"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(
        build,
        [
            "cargo",
            "build",
            "-p",
            "dbento-app",
            "--bin",
            "dbento-app",
            "--profile",
            "timing"
        ]
    );
    assert_eq!(session["binary"].as_str(), Some("target/timing/dbento-app"));
    assert!(
        session.get("refresh_hz").is_none(),
        "the default 144 Hz stays"
    );
}

#[test]
fn the_timing_profile_is_dev_at_opt_level_3() {
    let text = std::fs::read_to_string(repo_root().join("Cargo.toml")).unwrap();
    let manifest: toml::Table = toml::from_str(&text).unwrap();
    let timing = &manifest["profile"]["timing"];
    assert_eq!(timing["inherits"].as_str(), Some("dev"));
    assert_eq!(timing["opt-level"].as_integer(), Some(3));
}

#[test]
fn the_pilot_workspace_pins_its_host_tag_and_optimises_its_dependencies() {
    let text =
        std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")).unwrap();
    let manifest: toml::Table = toml::from_str(&text).unwrap();
    assert_eq!(
        manifest["dependencies"]["gpui-pilot-host"]["tag"].as_str(),
        Some("v0.2.0")
    );
    assert_eq!(
        manifest["profile"]["dev"]["package"]["*"]["opt-level"].as_integer(),
        Some(3)
    );
}

#[test]
fn the_project_registers_the_server_over_stdio() {
    let text = std::fs::read_to_string(repo_root().join(".mcp.json")).unwrap();
    let config: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        config["mcpServers"]["gpui-pilot"],
        serde_json::json!({ "command": "gpui-pilot", "args": ["mcp"] })
    );
}
