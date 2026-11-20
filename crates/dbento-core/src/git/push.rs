//! Sending the bound branch to `origin`, folding unpushed saves into one commit first, and the
//! counters a sync reads to decide whether a push is due.

use super::commit::{ensure_on_branch, failed, local, with_identity};
use super::{GitError, GitOptions, Refusal, SAVE_MESSAGE, SyncError, parse_refusal, run};
use crate::team::Binding;

/// How the server answered a push.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum PushOutcome {
    /// The server's branch now holds the local branch.
    Pushed,
    /// The server's branch has commits the local branch lacks; pull and try again.
    NonFastForward,
    /// The server's hook refused the push.
    Refused(Refusal),
}

/// Push the bound branch to the same branch on `origin`, never forced. A rejected update is an
/// outcome, not an error; anything else git fails on is `Git`.
pub fn push(binding: &Binding, opts: &GitOptions) -> Result<PushOutcome, SyncError> {
    ensure_on_branch(binding, opts)?;
    let full = format!("refs/heads/{}", binding.branch);
    let refspec = format!("{full}:{full}");
    let args = [
        "push",
        "--porcelain",
        "--no-recurse-submodules",
        "--no-follow-tags",
        "--",
        "origin",
        refspec.as_str(),
    ];
    let out = run::git(&args, &binding.dir, run::Reach::Network, opts)?;
    let stdout = String::from_utf8_lossy(&out.stdout);
    match porcelain_ref(&stdout) {
        Some((' ' | '*' | '=', _)) if out.status.success() => Ok(PushOutcome::Pushed),
        Some(('!', summary)) if summary.starts_with("[rejected]") => {
            Ok(PushOutcome::NonFastForward)
        }
        // A hook that printed no Potluck lines still refused; git's reason stands in for them.
        Some(('!', summary)) if summary.starts_with("[remote rejected]") => {
            let refusal = parse_refusal(&out.stderr).unwrap_or_else(|| Refusal {
                findings: Vec::new(),
                more: 0,
                other: vec![rejection_reason(summary).to_owned()],
            });
            Ok(PushOutcome::Refused(refusal))
        }
        _ => Err(failed(&args, out.stderr)),
    }
}

/// With more than one unpushed commit, replace them with one `SAVE_MESSAGE` commit of the same
/// tree on `origin/<branch>`, so blobs only the dropped commits held are never pushed. The index
/// is left alone. Returns whether `HEAD` moved; commits that cancel out leave the branch at
/// `origin/<branch>`.
pub fn squash_unpushed(binding: &Binding, opts: &GitOptions) -> Result<bool, SyncError> {
    let dir = binding.dir.as_path();
    ensure_on_branch(binding, opts)?;
    if unpushed(binding, opts)? <= 1 {
        return Ok(false);
    }
    let remote = format!("refs/remotes/origin/{}", binding.branch);
    let ancestor = ["merge-base", "--is-ancestor", remote.as_str(), "HEAD"];
    let out = run::git(&ancestor, dir, run::Reach::Local, opts)?;
    match out.status.code() {
        Some(0) => {}
        Some(1) => {
            return Err(SyncError::Diverged {
                branch: binding.branch.clone(),
            });
        }
        _ => return Err(failed(&ancestor, out.stderr)),
    }
    // Build the squash from the saved tree rather than the index, so anything staged by hand
    // stays staged and out of the commit. Saves that cancel out leave the remote tip itself.
    let old = rev(dir, "HEAD", opts)?;
    let old_tree = rev(dir, &format!("{old}^{{tree}}"), opts)?;
    let target = if old_tree == rev(dir, &format!("{remote}^{{tree}}"), opts)? {
        remote
    } else {
        let commit = [
            "commit-tree",
            old_tree.as_str(),
            "-p",
            remote.as_str(),
            "-m",
            SAVE_MESSAGE,
        ];
        let made = local(&commit, dir, &with_identity(binding, opts)?)?;
        String::from_utf8_lossy(&made).trim().to_owned()
    };
    // A failed move puts the branch back where it was, so nothing is left half squashed.
    if let Err(e) = local(&["reset", "-q", "--soft", target.as_str()], dir, opts) {
        local(&["reset", "-q", "--soft", old.as_str()], dir, opts)?;
        return Err(e);
    }
    Ok(true)
}

/// The full id `spec` names; it must name something.
fn rev(dir: &std::path::Path, spec: &str, opts: &GitOptions) -> Result<String, SyncError> {
    let out = local(&["rev-parse", "--verify", "-q", spec], dir, opts)?;
    Ok(String::from_utf8_lossy(&out).trim().to_owned())
}

/// How many commits of the bound branch `origin/<branch>` lacks, as the last fetch left it.
pub fn unpushed(binding: &Binding, opts: &GitOptions) -> Result<u32, SyncError> {
    let range = format!("refs/remotes/origin/{0}..refs/heads/{0}", binding.branch);
    let out = local(&["rev-list", "--count", range.as_str()], &binding.dir, opts)?;
    let text = String::from_utf8_lossy(&out);
    text.trim()
        .parse()
        .map_err(|_| GitError::Unexpected(format!("rev-list count: {}", text.trim())).into())
}

/// The server's tip of the bound branch, read with `ls-remote`; `None` when it has none.
pub fn remote_tip(binding: &Binding, opts: &GitOptions) -> Result<Option<String>, SyncError> {
    let full = format!("refs/heads/{}", binding.branch);
    let args = ["ls-remote", "--heads", "--", "origin", full.as_str()];
    let out = run::git_ok(&args, &binding.dir, run::Reach::Network, opts)?;
    // The pattern matches by suffix, so only the line naming exactly the bound ref counts.
    Ok(String::from_utf8_lossy(&out.stdout).lines().find_map(|l| {
        let (oid, name) = l.split_once('\t')?;
        (name == full).then(|| oid.to_owned())
    }))
}

/// The flag and summary of the one ref line in `push --porcelain` output: the line whose second
/// byte is a tab. The `To` and `Done` lines are skipped.
fn porcelain_ref(stdout: &str) -> Option<(char, &str)> {
    let line = stdout
        .lines()
        .find(|l| l.as_bytes().get(1) == Some(&b'\t'))?;
    let flag = line.chars().next()?;
    let summary = line.splitn(3, '\t').nth(2)?;
    Some((flag, summary.trim()))
}

/// The parenthesized reason after a rejection's status, such as `pre-receive hook declined` in
/// `[remote rejected] (pre-receive hook declined)`; the whole summary when there is none.
fn rejection_reason(summary: &str) -> &str {
    // The reason may hold parentheses itself, so read from the first `(` after the bracketed
    // status to the final `)`.
    let after = summary.find(']').map_or(0, |close| close + 1);
    let Some(open) = summary[after..].find('(') else {
        return summary;
    };
    let inner = &summary[after + open + 1..];
    inner.strip_suffix(')').unwrap_or(inner)
}
