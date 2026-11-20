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

pub const PUBLIC_URL: &str = "http://127.0.0.1:9";

/// The binding of workspace `dir` on `main` for user `ada`.
pub fn binding(dir: &Path) -> dbento_core::Binding {
    dbento_core::Binding::new(dir, PUBLIC_URL, "main").account(PUBLIC_URL, "ada")
}

impl Sandbox {
    /// Give the sandbox's global git config a user identity.
    pub fn set_identity(&self, name: &str, email: &str) {
        let text = format!("[user]\n\tname = {name}\n\temail = {email}\n");
        fs::write(self.path().join("gitconfig"), text).expect("gitconfig");
    }

    fn command(&self, cwd: &Path, args: &[&str]) -> Command {
        let mut cmd = Command::new("git");
        cmd.args(args).current_dir(cwd).envs(self.env());
        for who in ["AUTHOR", "COMMITTER"] {
            cmd.env(format!("GIT_{who}_NAME"), "Teammate");
            cmd.env(format!("GIT_{who}_EMAIL"), "mate@example.com");
        }
        for name in [
            "GIT_DIR",
            "GIT_WORK_TREE",
            "GIT_INDEX_FILE",
            "GIT_CONFIG_PARAMETERS",
        ] {
            cmd.env_remove(name);
        }
        cmd
    }

    /// Run the real git as a teammate in `cwd`, require success, return stdout.
    pub fn git(&self, cwd: &Path, args: &[&str]) -> String {
        let out = self.command(cwd, args).output().expect("git runs");
        let err = String::from_utf8_lossy(&out.stderr);
        assert!(out.status.success(), "git {args:?} failed: {err}");
        String::from_utf8(out.stdout).expect("utf-8 stdout")
    }

    pub fn rev(&self, repo: &Path, spec: &str) -> String {
        self.git(repo, &["rev-parse", spec]).trim().to_owned()
    }

    pub fn url(&self) -> String {
        format!("file://{}", self.path().join("origin.git").display())
    }

    /// A bare `origin.git` on `main` holding `dbento.toml` and `queries/a.sql`.
    pub fn origin(&self) {
        self.git(
            self.path(),
            &["init", "-q", "--bare", "-b", "main", "origin.git"],
        );
        self.git(self.path(), &["init", "-q", "-b", "main", "seed"]);
        let seed = self.path().join("seed");
        write(&seed, "dbento.toml", "name = \"seed\"\n");
        write(&seed, "queries/a.sql", "select 1;\n");
        self.git(&seed, &["add", "-A"]);
        self.git(&seed, &["commit", "-q", "-m", "seed"]);
        self.git(&seed, &["push", "-q", &self.url(), "main"]);
    }

    /// A plain clone of origin at `<sandbox>/<name>` on `main`.
    pub fn clone_as(&self, name: &str) -> PathBuf {
        self.git(
            self.path(),
            &["clone", "-q", "-b", "main", &self.url(), name],
        );
        self.path().join(name)
    }

    /// In teammate clone `repo`: write `rel` (delete it on `None`), commit and push.
    pub fn teammate(&self, repo: &Path, rel: &str, text: Option<&str>) {
        match text {
            Some(text) => write(repo, rel, text),
            None => fs::remove_file(repo.join(rel)).expect("remove"),
        }
        self.git(repo, &["add", "-A"]);
        self.git(repo, &["commit", "-q", "-m", &format!("teammate {rel}")]);
        self.git(repo, &["push", "-q", "origin", "main"]);
    }

    /// `rel` on origin's `main`, `None` when absent.
    pub fn origin_show(&self, rel: &str) -> Option<String> {
        let spec = format!("main:{rel}");
        let args = ["--git-dir", "origin.git", "show", spec.as_str()];
        let out = self.command(self.path(), &args).output().expect("git runs");
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// Every object reachable in origin, one per line.
    pub fn origin_objects(&self) -> String {
        self.git(
            self.path(),
            &["--git-dir", "origin.git", "rev-list", "--objects", "--all"],
        )
    }

    #[cfg(unix)]
    pub fn hook(&self, name: &str, body: &str) {
        self.script(&self.path().join("origin.git/hooks").join(name), body);
    }
}
