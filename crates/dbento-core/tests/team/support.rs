//! Hermetic git sandboxes: every git call and GitOptions built here reads an empty global
//! config, no system config and a home inside the sandbox.
#![allow(dead_code)]

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use dbento_core::GitOptions;
use tempfile::TempDir;

pub struct Sandbox {
    dir: TempDir,
}

impl Sandbox {
    pub fn new() -> Sandbox {
        let dir = TempDir::new().expect("tempdir");
        fs::create_dir(dir.path().join("home")).expect("home");
        fs::write(dir.path().join("gitconfig"), "").expect("gitconfig");
        Sandbox { dir }
    }

    pub fn path(&self) -> &Path {
        self.dir.path()
    }

    pub fn env(&self) -> Vec<(OsString, OsString)> {
        vec![
            ("HOME".into(), self.path().join("home").into()),
            (
                "GIT_CONFIG_GLOBAL".into(),
                self.path().join("gitconfig").into(),
            ),
            ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
            ("GIT_CEILING_DIRECTORIES".into(), self.path().into()),
            ("GIT_AUTHOR_DATE".into(), "2026-01-01T00:00:00Z".into()),
            ("GIT_COMMITTER_DATE".into(), "2026-01-01T00:00:00Z".into()),
        ]
    }

    /// Library options carrying `env()` and no identity, so the username fallback applies.
    pub fn options(&self) -> GitOptions {
        self.env()
            .into_iter()
            .fold(GitOptions::new(), |o, (k, v)| o.env(k, v))
    }

    /// An executable shell script at `path`, written by a child shell so no descriptor of
    /// this process is open on it when another test thread forks.
    #[cfg(unix)]
    pub fn script(&self, path: &Path, body: &str) {
        fs::create_dir_all(path.parent().expect("parent")).expect("script dir");
        let status = Command::new("sh")
            .args([
                "-c",
                "printf '%s' \"$1\" > \"$2\" && chmod 755 \"$2\"",
                "sh",
            ])
            .arg(format!("#!/bin/sh\n{body}\n"))
            .arg(path)
            .status()
            .expect("sh runs");
        assert!(status.success(), "script written");
    }

    /// A folder `dir` holding a fake `git`; returns a PATH value that finds it first.
    #[cfg(unix)]
    pub fn fake_git(&self, dir: &str, body: &str) -> String {
        let bin = self.path().join(dir);
        self.script(&bin.join("git"), body);
        format!("{}:/usr/bin:/bin", bin.display())
    }
}

pub fn write(root: &Path, rel: &str, text: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().expect("parent")).expect("dir");
    fs::write(path, text).expect("write");
}

pub fn read(root: &Path, rel: &str) -> Option<String> {
    fs::read_to_string(root.join(rel)).ok()
}

/// The absolute path of the real git.
pub fn real_git() -> PathBuf {
    let out = Command::new("sh")
        .args(["-c", "command -v git"])
        .output()
        .expect("sh runs");
    PathBuf::from(String::from_utf8(out.stdout).expect("utf-8").trim())
}
