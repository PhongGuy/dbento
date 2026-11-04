//! Repository automation for dbento, run as `cargo xtask`.

use std::path::PathBuf;
use std::process::Command;

use anyhow::{Context, bail};

pub mod app_bench;
pub mod bench;
pub mod datasets;
pub mod db;
pub mod demo;
pub mod icons;
pub mod notices;
pub mod packager_config;
pub mod stress;
pub mod windows_check;

/// The repository root, two levels above this crate's manifest.
pub fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|crates| crates.parent())
        .expect("xtask lives at crates/xtask")
        .to_path_buf()
}

/// `cargo metadata --no-deps --format-version 1` for the workspace, parsed.
pub fn cargo_metadata() -> anyhow::Result<serde_json::Value> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let manifest = workspace_root().join("Cargo.toml");
    let out = Command::new(&cargo)
        .args([
            "metadata",
            "--no-deps",
            "--format-version",
            "1",
            "--manifest-path",
        ])
        .arg(&manifest)
        .output()
        .context("running cargo metadata")?;
    if !out.status.success() {
        bail!(
            "cargo metadata failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    serde_json::from_slice(&out.stdout).context("parsing cargo metadata")
}
