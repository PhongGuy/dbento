//! Saving a workspace's own files as one commit on the bound branch, as the right person.

use std::path::Path;

use super::{GitError, GitOptions, SyncError, run};
use crate::team::Binding;

/// The paths a team workspace syncs; everything else in the folder stays local.
const WORKSPACE_PATHS: [&str; 3] = ["dbento.toml", "queries", ".gitignore"];

/// Commit every change to the workspace paths with `message`, leaving other files, staged or
/// not, alone. Returns whether a commit was made; refuses with `WrongBranch` before any write
/// when the folder is not cleanly on the bound branch.
pub fn commit_pending(
    binding: &Binding,
    message: &str,
    opts: &GitOptions,
) -> Result<bool, SyncError> {
    let dir = binding.dir.as_path();
    ensure_on_branch(binding, opts)?;
    // A path counts when it is on disk or tracked, so a deleted folder still commits as deletes.
    let mut paths = Vec::new();
    for path in WORKSPACE_PATHS {
        let on_disk = dir.join(path).symlink_metadata().is_ok();
        if on_disk || !local(&["ls-files", "-z", "--", path], dir, opts)?.is_empty() {
            paths.push(path);
        }
    }
    if paths.is_empty() {
        return Ok(false);
    }
    let with = |head: &[&str]| -> Vec<String> {
        head.iter()
            .chain(["--"].iter())
            .chain(paths.iter())
            .map(|s| (*s).to_owned())
            .collect()
    };
    local(&with(&["add", "-A"]), dir, opts)?;
    // Exit 1 from a quiet cached diff is the only sign that something is staged.
    let diff = with(&["diff", "--cached", "--quiet"]);
    let out = run::git(&diff, dir, run::Reach::Local, opts)?;
    match out.status.code() {
        Some(0) => return Ok(false),
        Some(1) => {}
        _ => {
            return Err(failed(&diff, out.stderr));
        }
    }
    // The pathspec limits the commit to these paths, so a file staged by hand stays staged.
    let commit = with(&["commit", "-q", "-m", message]);
    local(&commit, dir, &with_identity(binding, opts)?)?;
    Ok(true)
}

/// Refuse unless `HEAD` is the bound branch and no rebase, merge or cherry-pick is under way.
pub(crate) fn ensure_on_branch(binding: &Binding, opts: &GitOptions) -> Result<(), SyncError> {
    let dir = binding.dir.as_path();
    let wrong = |found: Option<String>| SyncError::WrongBranch {
        expected: binding.branch.clone(),
        found,
    };
    let args = ["symbolic-ref", "-q", "HEAD"];
    let out = run::git(&args, dir, run::Reach::Local, opts)?;
    let head = match out.status.code() {
        Some(0) => String::from_utf8_lossy(&out.stdout).trim().to_owned(),
        Some(1) => return Err(wrong(None)),
        _ => {
            return Err(failed(&args, out.stderr));
        }
    };
    if head != format!("refs/heads/{}", binding.branch) {
        let name = head.strip_prefix("refs/heads/").unwrap_or(&head);
        return Err(wrong(Some(name.to_owned())));
    }
    // Mid-operation the folder holds a half-applied state, so no step may build on it.
    let states = [
        "rebase-merge",
        "rebase-apply",
        "MERGE_HEAD",
        "CHERRY_PICK_HEAD",
    ];
    let mut args = vec!["rev-parse"];
    for state in states {
        args.extend(["--git-path", state]);
    }
    let paths = String::from_utf8_lossy(&local(&args, dir, opts)?).into_owned();
    if paths
        .lines()
        .any(|p| dir.join(p).symlink_metadata().is_ok())
    {
        return Err(wrong(None));
    }
    Ok(())
}

/// `opts` with the account as author and committer when git has no full identity configured;
/// a configured `user.name` and `user.email` win.
pub(crate) fn with_identity(binding: &Binding, opts: &GitOptions) -> Result<GitOptions, SyncError> {
    let dir = binding.dir.as_path();
    for key in ["user.name", "user.email"] {
        let args = ["config", "--get", key];
        let out = run::git(&args, dir, run::Reach::Local, opts)?;
        match out.status.code() {
            Some(0) => {}
            Some(1) => return Ok(account_identity(binding, opts.clone())),
            _ => {
                return Err(failed(&args, out.stderr));
            }
        }
    }
    Ok(opts.clone())
}

fn account_identity(binding: &Binding, opts: GitOptions) -> GitOptions {
    let name = binding.username.as_str();
    let email = format!("{name}@potluck.invalid");
    opts.env("GIT_AUTHOR_NAME", name)
        .env("GIT_AUTHOR_EMAIL", &email)
        .env("GIT_COMMITTER_NAME", name)
        .env("GIT_COMMITTER_EMAIL", &email)
}

/// A non-zero exit of `args` as an error.
fn failed<S: AsRef<std::ffi::OsStr>>(args: &[S], stderr: String) -> SyncError {
    GitError::Failed {
        command: run::command_line(args),
        stderr,
    }
    .into()
}

/// Run a local git call that must succeed and return its stdout.
fn local<S: AsRef<std::ffi::OsStr>>(
    args: &[S],
    dir: &Path,
    opts: &GitOptions,
) -> Result<Vec<u8>, SyncError> {
    Ok(run::git_ok(args, dir, run::Reach::Local, opts)?.stdout)
}
