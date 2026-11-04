//! `cargo xtask bench`: builds and runs the release bench.

use anyhow::{Context, Result};
use std::process::Command;

/// The cargo arguments that build the release bench, so a failed build is told apart from a run.
pub fn build_args() -> Vec<String> {
    [
        "build",
        "--release",
        "--locked",
        "--quiet",
        "-p",
        "dbento-bench",
    ]
    .map(String::from)
    .into()
}

/// The cargo arguments that run the bench's `run` subcommand with `extra` flags.
pub fn cargo_args(extra: &[String]) -> Vec<String> {
    [
        "run",
        "--release",
        "--locked",
        "--quiet",
        "-p",
        "dbento-bench",
        "--",
        "run",
    ]
    .into_iter()
    .map(String::from)
    .chain(extra.iter().cloned())
    .collect()
}

/// Runs the bench and returns its exit code (0 met, 1 missed, 2 setup error). A failed build,
/// or a bench that ends outside that contract (a panic's 101, a signal), is 2.
pub fn run(extra: &[String]) -> Result<i32> {
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let built = Command::new(&cargo)
        .args(build_args())
        .status()
        .with_context(|| format!("spawning {cargo}"))?;
    if !built.success() {
        eprintln!("error: the release build of dbento-bench failed ({built})");
        return Ok(2);
    }
    let status = Command::new(&cargo)
        .args(cargo_args(extra))
        .status()
        .with_context(|| format!("spawning {cargo}"))?;
    Ok(match status.code() {
        Some(code @ 0..=2) => code,
        _ => {
            eprintln!("error: the bench ended abnormally ({status})");
            2
        }
    })
}
