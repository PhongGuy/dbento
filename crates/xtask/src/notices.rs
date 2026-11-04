//! `cargo xtask notices`: render the third-party notices page through cargo-about.

use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, bail};

pub const ABOUT_VERSION: &str = "0.9.2";
pub const INSTALL: &str = "cargo install --locked cargo-about --version 0.9.2 --features cli";

/// True when `cargo about --version` output, trimmed, is exactly "cargo-about 0.9.2".
pub fn version_ok(version_output: &str) -> bool {
    version_output.trim() == format!("cargo-about {ABOUT_VERSION}")
}

/// The `cargo` arguments that render `<root>/about.hbs` into `out` under `<root>/about.toml`.
pub fn generate_args(root: &Path, out: &Path) -> Vec<OsString> {
    let mut args: Vec<OsString> = [
        "about",
        "generate",
        "--workspace",
        "--locked",
        "--offline",
        "--fail",
        "--config",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    args.push(root.join("about.toml").into());
    args.push("--output-file".into());
    args.push(out.into());
    args.push(root.join("about.hbs").into());
    args
}

/// Gate on the cargo-about version (Err naming the found version and INSTALL), then run
/// `$CARGO about generate` from the workspace root with inherited stderr; Ok(1) when it fails.
/// `check` writes into a temporary directory that is removed; otherwise `out` receives the page.
pub fn run(check: bool, out: Option<&Path>) -> anyhow::Result<i32> {
    let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
    let root = crate::workspace_root();

    // A cargo-about that cannot run at all reports as "none" rather than a spawn error.
    let found = Command::new(&cargo)
        .args(["about", "--version"])
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).into_owned())
        .unwrap_or_default();
    if !version_ok(&found) {
        let first = found.lines().next().unwrap_or("").trim();
        let first = if first.is_empty() { "none" } else { first };
        bail!("need cargo-about {ABOUT_VERSION} (found: {first}); run: {INSTALL}");
    }

    // The temporary directory lives until this function returns, so the check leaves nothing.
    let scratch = tempfile::tempdir().context("creating a temporary directory")?;
    // cargo-about runs from the root, so a relative --out is anchored to our directory first.
    let target = match (check, out) {
        (false, Some(out)) => std::path::absolute(out).context("resolving --out")?,
        _ => scratch.path().join("THIRD-PARTY-NOTICES.html"),
    };
    let status = Command::new(&cargo)
        .args(generate_args(&root, &target))
        .current_dir(&root)
        .status()
        .with_context(|| format!("running {} about generate", cargo.to_string_lossy()))?;
    Ok(i32::from(!status.success()))
}
