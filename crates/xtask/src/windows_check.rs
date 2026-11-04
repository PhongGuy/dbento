//! `cargo xtask check-windows`: type-check the headless crates for Windows via mingw.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::Context;

pub const TARGET: &str = "x86_64-pc-windows-gnu";
pub const CROSS_CC: &str = "x86_64-w64-mingw32-gcc";
pub const PACKAGES: [&str; 3] = ["dbento-core", "dbento-cli", "dbento-mcp"];

/// Whether the cross-check can run, or why it is skipped.
#[derive(Debug, PartialEq, Eq)]
pub enum Plan {
    Run,
    Skip(String),
}

/// The first executable file named `tool` in the `PATH`-style list `path`.
pub fn find_on_path(tool: &str, path: &OsStr) -> Option<PathBuf> {
    std::env::split_paths(path)
        .map(|dir| dir.join(tool))
        .find(|candidate| is_executable(candidate))
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    path.is_file()
}

/// True when the sysroot carries the Windows GNU standard library.
pub fn target_installed(sysroot: &Path) -> bool {
    sysroot.join("lib/rustlib").join(TARGET).is_dir()
}

/// Run only when both the cross compiler and the target std are present; otherwise
/// name every missing piece with the command that installs it.
pub fn plan(cross_cc: Option<&Path>, target_installed: bool) -> Plan {
    let mut missing = Vec::new();
    if cross_cc.is_none() {
        missing.push(format!(
            "{CROSS_CC} not found; install mingw-w64-gcc (Arch) or gcc-mingw-w64-x86-64 (Debian, Ubuntu)"
        ));
    }
    if !target_installed {
        missing.push(format!(
            "target {TARGET} not installed; run `rustup target add {TARGET}`"
        ));
    }
    if missing.is_empty() {
        Plan::Run
    } else {
        Plan::Skip(format!(
            "skipped Windows cross-check: {}",
            missing.join("; ")
        ))
    }
}

/// The rustc whose sysroot is checked: `RUSTC` from `get` when non-empty, as cargo would use,
/// else `rustc` from PATH.
pub fn rustc_program(get: impl Fn(&str) -> Option<OsString>) -> OsString {
    get("RUSTC")
        .filter(|rustc| !rustc.is_empty())
        .unwrap_or_else(|| "rustc".into())
}

/// The `cargo` arguments that type-check the crates without a GUI dependency.
pub fn cargo_args() -> Vec<String> {
    let mut args = vec!["check".to_owned(), "--target".to_owned(), TARGET.to_owned()];
    for package in PACKAGES {
        args.extend(["-p".to_owned(), package.to_owned()]);
    }
    // The mingw check leaves DuckDB out, so CI skips a second libduckdb build, but keeps the keychain.
    args.extend(
        [
            "--no-default-features",
            "--features",
            "dbento-core/keychain",
        ]
        .map(str::to_owned),
    );
    args
}

/// Run the check, or report the skip: Ok(0) on a pass or a plain skip, Ok(1) on a failed
/// check or, with `require` (CI), on a skip.
pub fn run(require: bool) -> anyhow::Result<i32> {
    let path = std::env::var_os("PATH").unwrap_or_default();
    let cross_cc = find_on_path(CROSS_CC, &path);
    // A rustc that cannot run (or an unreadable sysroot) means the target is unusable.
    let installed = Command::new(rustc_program(|name| std::env::var_os(name)))
        .args(["--print", "sysroot"])
        .output()
        .ok()
        .filter(|out| out.status.success())
        .is_some_and(|out| {
            target_installed(Path::new(String::from_utf8_lossy(&out.stdout).trim()))
        });

    match plan(cross_cc.as_deref(), installed) {
        Plan::Skip(reason) => {
            eprintln!("{reason}");
            Ok(i32::from(require))
        }
        Plan::Run => {
            let cargo = std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into());
            let status = Command::new(&cargo)
                .args(cargo_args())
                .current_dir(crate::workspace_root())
                .status()
                .with_context(|| format!("running {}", cargo.to_string_lossy()))?;
            Ok(i32::from(!status.success()))
        }
    }
}
