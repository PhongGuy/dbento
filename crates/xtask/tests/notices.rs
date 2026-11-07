//! `cargo xtask notices`: the cargo-about version gate, its arguments and the committed config.
#![cfg(unix)]

use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use xtask::notices::{ABOUT_VERSION, INSTALL, generate_args, version_ok};

// A stand-in cargo: logs each call, answers `about --version` with FAKE_ABOUT_VERSION, and
// for `about generate` writes the --output-file or fails naming a crate when FAKE_FAIL is set.
const FAKE: &str = r#"#!/usr/bin/env bash
echo "$*" >>"$FAKE_LOG"
if [[ $1 == about && $2 == --version ]]; then echo "cargo-about $FAKE_ABOUT_VERSION"; exit 0; fi
if [[ $1 == about && $2 == generate ]]; then
  if [[ -n ${FAKE_FAIL:-} ]]; then
    echo "error: gplonly 1.0.0 has unaccepted license GPL-3.0-only" >&2
    exit 1
  fi
  while [[ $# -gt 0 ]]; do
    [[ $1 == --output-file ]] && printf '<html>notices</html>' >"$2"
    shift
  done
  exit 0
fi
exit 3
"#;

struct Fake {
    dir: tempfile::TempDir,
}

impl Fake {
    fn new() -> Fake {
        let dir = tempfile::tempdir().unwrap();
        let script = dir.path().join("cargo");
        std::fs::write(&script, FAKE).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
        Fake { dir }
    }

    fn run(&self, version: &str, fail: bool, args: &[OsString]) -> Output {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_xtask"));
        cmd.arg("notices")
            .args(args)
            .env("CARGO", self.dir.path().join("cargo"))
            .env("FAKE_LOG", self.dir.path().join("log"))
            .env("FAKE_ABOUT_VERSION", version);
        if fail {
            cmd.env("FAKE_FAIL", "1");
        } else {
            cmd.env_remove("FAKE_FAIL");
        }
        cmd.output().unwrap()
    }

    fn log(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.path().join("log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_owned)
            .collect()
    }

    fn out(&self) -> PathBuf {
        self.dir.path().join("THIRD-PARTY-NOTICES.html")
    }
}

fn out_arg(path: &Path) -> Vec<OsString> {
    vec!["--out".into(), path.into()]
}

#[test]
fn version_ok_wants_exactly_0_9_2() {
    assert_eq!(ABOUT_VERSION, "0.9.2");
    assert!(version_ok("cargo-about 0.9.2\n"));
    assert!(!version_ok("cargo-about 0.8.4\n"));
    assert!(!version_ok("cargo-about 0.9.20"));
    assert!(!version_ok("cargo-about 0.9.2-rc1"));
    assert!(!version_ok(""));
}

#[test]
fn generate_args_name_the_committed_config_and_template() {
    let root = Path::new("/r");
    let want: Vec<OsString> = [
        "about",
        "generate",
        "--workspace",
        "--locked",
        "--offline",
        "--fail",
        "--config",
        "/r/about.toml",
        "--output-file",
        "/o/n.html",
        "/r/about.hbs",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    assert_eq!(generate_args(root, Path::new("/o/n.html")), want);
}

#[test]
fn an_old_cargo_about_is_refused_with_the_install_line() {
    let fake = Fake::new();
    let out = fake.run("0.8.4", false, &out_arg(&fake.out()));
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains(INSTALL) && stderr.contains("0.8.4"),
        "{stderr}"
    );
    assert_eq!(fake.log(), ["about --version"], "generate must not run");
    assert!(!fake.out().exists());
}

#[test]
fn out_receives_the_generated_page() {
    let fake = Fake::new();
    let out = fake.run("0.9.2", false, &out_arg(&fake.out()));
    assert!(out.status.success(), "{out:?}");
    assert_eq!(
        std::fs::read_to_string(fake.out()).unwrap(),
        "<html>notices</html>"
    );
    let log = fake.log();
    assert_eq!(log.len(), 2, "{log:?}");
    assert!(
        log[1].starts_with("about generate --workspace --locked --offline --fail --config "),
        "{}",
        log[1]
    );
    assert!(log[1].ends_with("/about.hbs"), "{}", log[1]);
}

// cargo-about runs from the workspace root, so a relative --out must be resolved against
// the caller's directory first, or the page lands under the root instead.
#[test]
fn a_relative_out_is_written_beside_the_caller() {
    let fake = Fake::new();
    let name = "m6b-relative-out-test.html";
    let stray = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(name);
    let out = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["notices", "--out", name])
        .current_dir(fake.dir.path())
        .env("CARGO", fake.dir.path().join("cargo"))
        .env("FAKE_LOG", fake.dir.path().join("log"))
        .env("FAKE_ABOUT_VERSION", "0.9.2")
        .env_remove("FAKE_FAIL")
        .output()
        .unwrap();
    let landed_in_root = stray.exists();
    let _ = std::fs::remove_file(&stray);
    assert!(out.status.success(), "{out:?}");
    assert!(!landed_in_root, "page written under the workspace root");
    assert_eq!(
        std::fs::read_to_string(fake.dir.path().join(name)).unwrap(),
        "<html>notices</html>"
    );
}

#[test]
fn check_generates_and_keeps_nothing() {
    let fake = Fake::new();
    let out = fake.run("0.9.2", false, &["--check".into()]);
    assert!(out.status.success(), "{out:?}");
    let log = fake.log();
    let written = log[1]
        .split(" --output-file ")
        .nth(1)
        .and_then(|rest| rest.split(' ').next())
        .expect("generate got --output-file");
    assert!(!Path::new(written).exists(), "{written} left behind");
    assert!(
        !Path::new(written).parent().unwrap().exists(),
        "temporary directory left behind"
    );
}

#[test]
fn an_unaccepted_licence_fails_and_names_the_crate() {
    let fake = Fake::new();
    let out = fake.run("0.9.2", true, &["--check".into()]);
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(String::from_utf8_lossy(&out.stderr).contains("gplonly"));
}

#[test]
fn out_is_required_without_check() {
    let fake = Fake::new();
    let out = fake.run("0.9.2", false, &[]);
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    assert!(fake.log().is_empty());
}

#[test]
fn about_toml_accepts_the_spec_list_for_the_release_targets() {
    let text = std::fs::read_to_string(xtask::workspace_root().join("about.toml")).unwrap();
    let about: toml::Table = text.parse().unwrap();
    let list = |key: &str| -> Vec<String> {
        about[key]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_owned())
            .collect()
    };
    assert_eq!(
        list("accepted"),
        [
            "MIT",
            "Apache-2.0",
            "BSD-2-Clause",
            "BSD-3-Clause",
            "ISC",
            "Zlib",
            "0BSD",
            "MPL-2.0",
            "Unicode-3.0",
            "CC0-1.0",
            "bzip2-1.0.6",
            "CDLA-Permissive-2.0",
            "BSL-1.0"
        ]
    );
    assert_eq!(
        list("targets"),
        [
            "x86_64-unknown-linux-gnu",
            "aarch64-unknown-linux-gnu",
            "aarch64-apple-darwin",
            "x86_64-apple-darwin",
            "x86_64-pc-windows-msvc"
        ]
    );
    assert_eq!(about["ignore-dev-dependencies"].as_bool(), Some(true));
}

#[test]
fn the_template_lists_every_licence_with_its_text_and_users() {
    let hbs = std::fs::read_to_string(xtask::workspace_root().join("about.hbs")).unwrap();
    for want in [
        "{{#each licenses}}",
        "{{#each used_by}}",
        "{{crate.name}}",
        "{{text}}",
        "{{#each overview}}",
    ] {
        assert!(hbs.contains(want), "{want} missing from about.hbs");
    }
}
