//! Pull by rebasing unpushed commits onto the server's branch, with a fallback for a rebase
//! that stops: the local bytes of every conflicted path go to the conflict store before the
//! branch is reset to the remote tip, and the local changes that did not conflict are
//! committed again on top.

use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Write};
use std::path::Path;

use super::commit::{commit_pending, ensure_on_branch, failed, local, with_identity};
use super::{GitError, GitOptions, SAVE_MESSAGE, SyncError, run};
use crate::team::{Binding, ConflictStore};

/// The message of the commit that re-applies the local changes after a conflict.
const FALLBACK_MESSAGE: &str = "Keep local changes";

/// What [`pull_rebase`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Pulled {
    /// The server's branch had nothing the local branch lacks.
    UpToDate,
    /// Nothing was unpushed; the branch now sits at the server's tip.
    FastForwarded,
    /// The unpushed commits were replayed on the server's tip.
    Rebased,
    /// The rebase stopped; the local side of these paths is in the store, and the branch is
    /// the server's tip plus at most one commit of the other local changes.
    Conflicted(Vec<ConflictFile>),
}

/// One path the local and remote sides changed differently.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct ConflictFile {
    /// The path relative to the workspace, `/`-separated.
    pub path: String,
    /// How the two sides differ.
    pub kind: ConflictKind,
}

/// How the two sides of a conflicted path differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum ConflictKind {
    /// Both sides changed the file; the local bytes are kept as a copy.
    BothChanged,
    /// The local side deleted a file the remote changed; the remote file stays and a mark
    /// records the delete.
    DeletedLocally,
    /// The remote deleted a file the local side changed; the local bytes are kept as a copy.
    DeletedRemotely,
}

/// Save pending workspace changes, fetch the bound branch from `origin` and bring the local
/// branch onto it: a fast-forward when nothing is unpushed, otherwise a rebase. A rebase that
/// stops on a conflict falls back to the remote tip, keeping each conflicted path's local side
/// in `store`; one git refuses before starting is an error and changes nothing.
pub fn pull_rebase(
    binding: &Binding,
    store: &ConflictStore,
    opts: &GitOptions,
) -> Result<Pulled, SyncError> {
    let dir = binding.dir.as_path();
    ensure_on_branch(binding, opts)?;
    commit_pending(binding, SAVE_MESSAGE, opts)?;
    fetch(binding, opts)?;
    let branch = &binding.branch;
    let (local_ref, remote_ref) = (
        format!("refs/heads/{branch}"),
        format!("refs/remotes/origin/{branch}"),
    );
    let (ahead, behind) = counts(dir, &local_ref, &remote_ref, opts)?;
    if behind == 0 {
        return Ok(Pulled::UpToDate);
    }
    if ahead == 0 {
        local(
            &["merge", "-q", "--ff-only", remote_ref.as_str()],
            dir,
            opts,
        )?;
        return Ok(Pulled::FastForwarded);
    }
    // Pin both tips by id, so the fallback compares exactly what the rebase saw.
    let mine = rev(dir, &local_ref, opts)?.ok_or_else(|| unexpected("no local branch"))?;
    let theirs = rev(dir, &remote_ref, opts)?.ok_or_else(|| unexpected("no remote branch"))?;
    let args = [
        "rebase",
        "--no-autostash",
        "--no-autosquash",
        "--no-update-refs",
        theirs.as_str(),
    ];
    let out = run::git(
        &args,
        dir,
        run::Reach::Local,
        &with_identity(binding, opts)?,
    )?;
    if out.status.success() {
        return Ok(Pulled::Rebased);
    }
    // Only a rebase that left its state folder stopped on a conflict; one refused before it
    // began, such as over a dirty tree, changed nothing and is reported as git said.
    let state = local(
        &[
            "rev-parse",
            "--git-path",
            "rebase-merge",
            "--git-path",
            "rebase-apply",
        ],
        dir,
        opts,
    )?;
    let stopped = String::from_utf8_lossy(&state)
        .lines()
        .any(|p| dir.join(p).symlink_metadata().is_ok());
    if !stopped {
        return Err(failed(&args, out.stderr));
    }
    local(&["rebase", "--abort"], dir, opts)?;
    fall_back(binding, &mine, &theirs, store, opts)
}

/// Fetch the bound branch from `origin` into its remote-tracking ref.
pub(crate) fn fetch(binding: &Binding, opts: &GitOptions) -> Result<(), SyncError> {
    let spec = format!("+refs/heads/{0}:refs/remotes/origin/{0}", binding.branch);
    let args = ["fetch", "-q", "--no-tags", "origin", spec.as_str()];
    run::git_ok(&args, &binding.dir, run::Reach::Network, opts)?;
    Ok(())
}

/// After an aborted rebase: keep each conflicted path's local side in the store, and only
/// once all of them are on disk reset to `theirs` and commit every other local change again.
fn fall_back(
    binding: &Binding,
    mine: &str,
    theirs: &str,
    store: &ConflictStore,
    opts: &GitOptions,
) -> Result<Pulled, SyncError> {
    let dir = binding.dir.as_path();
    let base = String::from_utf8_lossy(&local(&["merge-base", mine, theirs], dir, opts)?)
        .trim()
        .to_owned();
    let my_paths = changed(dir, &base, mine, opts)?;
    let their_paths = changed(dir, &base, theirs, opts)?;
    let mut files = Vec::new();
    for path in my_paths.intersection(&their_paths) {
        let (l, r) = (blob(dir, mine, path, opts)?, blob(dir, theirs, path, opts)?);
        let kind = match (&l, &r) {
            _ if l == r => continue,
            (None, _) => ConflictKind::DeletedLocally,
            (_, None) => ConflictKind::DeletedRemotely,
            _ => ConflictKind::BothChanged,
        };
        match &l {
            // The newest conflict on a path wins, so a copy replaces an older delete's mark.
            Some(id) => {
                keep_copy(dir, id, &store.copy_path(path), opts)?;
                match fs::remove_file(store.mark_path(path)) {
                    Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e.into()),
                    _ => {}
                }
            }
            None => mark_deleted(&store.mark_path(path))?,
        }
        files.push(ConflictFile {
            path: path.clone(),
            kind,
        });
    }
    local(&["reset", "-q", "--hard", theirs], dir, opts)?;

    // Every local change outside the conflicts goes back on top of the remote tip.
    let (mut restore, mut remove) = (vec![], vec![]);
    for path in my_paths
        .iter()
        .filter(|p| !files.iter().any(|c| &c.path == *p))
    {
        match (blob(dir, mine, path, opts)?, blob(dir, theirs, path, opts)?) {
            (l, r) if l == r => {}
            (Some(_), _) => restore.push(path.as_str()),
            (None, Some(_)) => remove.push(path.as_str()),
            (None, None) => {}
        }
    }
    if !restore.is_empty() {
        local(
            &[&["checkout", mine, "--"], &restore[..]].concat(),
            dir,
            opts,
        )?;
    }
    if !remove.is_empty() {
        local(&[&["rm", "-q", "--"], &remove[..]].concat(), dir, opts)?;
    }
    if !restore.is_empty() || !remove.is_empty() {
        let commit = ["commit", "-q", "-m", FALLBACK_MESSAGE];
        local(&commit, dir, &with_identity(binding, opts)?)?;
    }
    Ok(Pulled::Conflicted(files))
}

/// Commits only on `left` and only on `right`.
fn counts(
    dir: &Path,
    left: &str,
    right: &str,
    opts: &GitOptions,
) -> Result<(usize, usize), SyncError> {
    let range = format!("{left}...{right}");
    let out = local(&["rev-list", "--left-right", "--count", &range], dir, opts)?;
    let text = String::from_utf8_lossy(&out);
    let mut parts = text.split_whitespace().map(str::parse::<usize>);
    match (parts.next(), parts.next(), parts.next()) {
        (Some(Ok(ahead)), Some(Ok(behind)), None) => Ok((ahead, behind)),
        _ => Err(unexpected(&format!("rev-list counts: {}", text.trim()))),
    }
}

/// The paths that differ between `from` and `to`, renames counted as a delete and an add. A
/// path that is not UTF-8 is an error, so no conflicted file is ever skipped by a lossy name.
fn changed(
    dir: &Path,
    from: &str,
    to: &str,
    opts: &GitOptions,
) -> Result<BTreeSet<String>, SyncError> {
    let args = ["diff", "--name-only", "--no-renames", "-z", from, to];
    local(&args, dir, opts)?
        .split(|&b| b == 0)
        .filter(|p| !p.is_empty())
        .map(|p| {
            String::from_utf8(p.to_vec()).map_err(|_| {
                unexpected(&format!(
                    "path is not UTF-8: {}",
                    String::from_utf8_lossy(p)
                ))
            })
        })
        .collect()
}

/// The blob id of `path` in commit `tip`, `None` when the path is not there.
fn blob(dir: &Path, tip: &str, path: &str, opts: &GitOptions) -> Result<Option<String>, SyncError> {
    rev(dir, &format!("{tip}:{path}"), opts)
}

/// The full id `spec` names, `None` when it names nothing.
fn rev(dir: &Path, spec: &str, opts: &GitOptions) -> Result<Option<String>, SyncError> {
    let args = ["rev-parse", "-q", "--verify", spec];
    let out = run::git(&args, dir, run::Reach::Local, opts)?;
    match out.status.code() {
        Some(0) => Ok(Some(String::from_utf8_lossy(&out.stdout).trim().to_owned())),
        Some(1) => Ok(None),
        _ => Err(failed(&args, out.stderr)),
    }
}

/// Write blob `id` to `dest` through a synced temporary file beside it, replacing an older copy.
fn keep_copy(dir: &Path, id: &str, dest: &Path, opts: &GitOptions) -> Result<(), SyncError> {
    let bytes = local(&["cat-file", "blob", id], dir, opts)?;
    let parent = dest
        .parent()
        .ok_or_else(|| io::Error::other("a copy needs a parent folder"))?;
    fs::create_dir_all(parent)?;
    let mut tmp = tempfile::NamedTempFile::new_in(parent)?;
    tmp.write_all(&bytes)?;
    tmp.as_file().sync_all()?;
    tmp.persist(dest).map_err(|e| e.error)?;
    Ok(())
}

/// Record one conflicted local delete; the mark is empty, its path is the whole message.
fn mark_deleted(mark: &Path) -> Result<(), SyncError> {
    let parent = mark
        .parent()
        .ok_or_else(|| io::Error::other("a mark needs a parent folder"))?;
    fs::create_dir_all(parent)?;
    fs::File::create(mark)?.sync_all()?;
    Ok(())
}

fn unexpected(what: &str) -> SyncError {
    GitError::Unexpected(what.to_owned()).into()
}
