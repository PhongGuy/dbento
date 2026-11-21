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
use crate::team::{Binding, ConflictStore, merge_workspace_text};
use crate::workspace::Workspace;

/// The message of the commit that re-applies the local changes after a conflict.
const FALLBACK_MESSAGE: &str = "Keep local changes";
/// The message of the commit that settles dbento.toml after a clean rebase.
const MERGE_MESSAGE: &str = "Merge dbento.toml";
/// The workspace file merged per entry instead of by line.
const TOML: &str = "dbento.toml";

/// What a pull does with dbento.toml once the rebase is over, decided before it starts.
enum TomlPlan {
    /// Not changed on both sides, changed identically, or deleted on one: git's result stands.
    Untouched,
    /// The merged text, already accepted by `Workspace::from_text`.
    Merged(String),
    /// The merge or its validation failed: theirs wins and mine goes to the store.
    Failed,
}

/// What [`pull_rebase`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Pulled {
    /// The server's branch had nothing the local branch lacks.
    UpToDate,
    /// Nothing was unpushed; the branch now sits at the server's tip.
    FastForwarded,
    /// The unpushed commits were replayed on the server's tip, or their changes were brought
    /// onto it with dbento.toml merged per entry.
    Rebased,
    /// These paths changed differently on both sides; their local side is in the store, and
    /// the branch is the server's tip plus at most one commit of the other local changes.
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
    // Decide dbento.toml from objects alone, so a rebase git refuses still changes nothing. A
    // missing merge-base leaves the file to git and the fallback, as without the merge.
    let base = merge_base(dir, &mine, &theirs, opts)?;
    let plan = match &base {
        Some(base) => plan_toml(dir, base, &mine, &theirs, opts)?,
        None => TomlPlan::Untouched,
    };
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
        return settle_rebased(binding, &mine, &theirs, plan, store, opts);
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
    let base = match base {
        Some(base) => base,
        None => String::from_utf8_lossy(&local(&["merge-base", &mine, &theirs], dir, opts)?)
            .trim()
            .to_owned(),
    };
    fall_back(binding, &mine, &theirs, &base, plan, store, opts)
}

/// The merge-base of `mine` and `theirs`, `None` when git names none.
fn merge_base(
    dir: &Path,
    mine: &str,
    theirs: &str,
    opts: &GitOptions,
) -> Result<Option<String>, SyncError> {
    let out = run::git(&["merge-base", mine, theirs], dir, run::Reach::Local, opts)?;
    let id = String::from_utf8_lossy(&out.stdout).trim().to_owned();
    Ok((out.status.success() && !id.is_empty()).then_some(id))
}

/// Decide dbento.toml before the rebase: merge it per entry when both sides changed it
/// differently, and keep the result only when it opens as a workspace.
fn plan_toml(
    dir: &Path,
    base: &str,
    mine: &str,
    theirs: &str,
    opts: &GitOptions,
) -> Result<TomlPlan, SyncError> {
    let base_id = blob(dir, base, TOML, opts)?;
    let (Some(mine_id), Some(their_id)) =
        (blob(dir, mine, TOML, opts)?, blob(dir, theirs, TOML, opts)?)
    else {
        return Ok(TomlPlan::Untouched);
    };
    let unchanged = |id: &String| base_id.as_ref() == Some(id);
    if unchanged(&mine_id) || unchanged(&their_id) || mine_id == their_id {
        return Ok(TomlPlan::Untouched);
    }
    // A blob that cannot be read as UTF-8 text cannot be merged, only chosen between.
    let text = |id: Option<&str>| -> Option<String> {
        let Some(id) = id else {
            return Some(String::new());
        };
        let out = run::git(&["cat-file", "blob", id], dir, run::Reach::Local, opts).ok()?;
        out.status.success().then_some(())?;
        String::from_utf8(out.stdout).ok()
    };
    let (Some(b), Some(m), Some(t)) = (
        text(base_id.as_deref()),
        text(Some(&mine_id)),
        text(Some(&their_id)),
    ) else {
        return Ok(TomlPlan::Failed);
    };
    Ok(match merge_workspace_text(&b, &m, &t) {
        Ok(merged) if Workspace::from_text(dir.to_path_buf(), &dir.join(TOML), &merged).is_ok() => {
            TomlPlan::Merged(merged)
        }
        _ => TomlPlan::Failed,
    })
}

/// After a successful rebase: write the merged text, or restore theirs and keep mine in
/// `store`, then commit when the index differs from `HEAD`.
fn settle_rebased(
    binding: &Binding,
    mine: &str,
    theirs: &str,
    plan: TomlPlan,
    store: &ConflictStore,
    opts: &GitOptions,
) -> Result<Pulled, SyncError> {
    let dir = binding.dir.as_path();
    let mut files = Vec::new();
    match plan {
        TomlPlan::Untouched => return Ok(Pulled::Rebased),
        TomlPlan::Merged(text) => {
            fs::write(dir.join(TOML), text)?;
            local(&["add", "--", TOML], dir, opts)?;
        }
        TomlPlan::Failed => {
            let id =
                blob(dir, mine, TOML, opts)?.ok_or_else(|| unexpected("no local dbento.toml"))?;
            keep_local(dir, &id, store, TOML, opts)?;
            local(&["checkout", theirs, "--", TOML], dir, opts)?;
            files.push(ConflictFile {
                path: TOML.to_owned(),
                kind: ConflictKind::BothChanged,
            });
        }
    }
    commit_if_staged(binding, MERGE_MESSAGE, opts)?;
    Ok(if files.is_empty() {
        Pulled::Rebased
    } else {
        Pulled::Conflicted(files)
    })
}

/// Commit the index as `message` when it differs from `HEAD`; returns whether it did.
fn commit_if_staged(
    binding: &Binding,
    message: &str,
    opts: &GitOptions,
) -> Result<bool, SyncError> {
    let dir = binding.dir.as_path();
    // Exit 1 from a quiet cached diff is the only sign that something is staged.
    let diff = ["diff", "--cached", "--quiet"];
    let out = run::git(&diff, dir, run::Reach::Local, opts)?;
    match out.status.code() {
        Some(0) => return Ok(false),
        Some(1) => {}
        _ => return Err(failed(&diff, out.stderr)),
    }
    local(
        &["commit", "-q", "-m", message],
        dir,
        &with_identity(binding, opts)?,
    )?;
    Ok(true)
}

/// Keep blob `id` as the local side of `path` in `store`. The newest conflict on a path wins,
/// so the copy replaces an older delete's mark.
fn keep_local(
    dir: &Path,
    id: &str,
    store: &ConflictStore,
    path: &str,
    opts: &GitOptions,
) -> Result<(), SyncError> {
    keep_copy(dir, id, &store.copy_path(path), opts)?;
    match fs::remove_file(store.mark_path(path)) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e.into()),
        _ => Ok(()),
    }
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
/// A merged dbento.toml is no conflict: its text is written over theirs and committed too.
fn fall_back(
    binding: &Binding,
    mine: &str,
    theirs: &str,
    base: &str,
    plan: TomlPlan,
    store: &ConflictStore,
    opts: &GitOptions,
) -> Result<Pulled, SyncError> {
    let dir = binding.dir.as_path();
    let merged = match plan {
        TomlPlan::Merged(text) => Some(text),
        TomlPlan::Untouched | TomlPlan::Failed => None,
    };
    let mut my_paths = changed(dir, base, mine, opts)?;
    if merged.is_some() {
        my_paths.remove(TOML);
    }
    let their_paths = changed(dir, base, theirs, opts)?;
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
            Some(id) => keep_local(dir, id, store, path, opts)?,
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
    // Written last, so no restore can overwrite the merge.
    if let Some(text) = merged {
        fs::write(dir.join(TOML), text)?;
        local(&["add", "--", TOML], dir, opts)?;
    }
    commit_if_staged(binding, FALLBACK_MESSAGE, opts)?;
    Ok(if files.is_empty() {
        Pulled::Rebased
    } else {
        Pulled::Conflicted(files)
    })
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
