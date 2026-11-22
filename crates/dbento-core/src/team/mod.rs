//! Team mode: a workspace folder bound to one branch of a Potluck server's repository.

mod account;
mod api;
mod conflicts;
mod flows;
#[expect(
    dead_code,
    reason = "the sync driver is the machine's caller and the only sender of StreamDown"
)]
mod machine;
#[cfg_attr(
    not(test),
    expect(dead_code, reason = "the sync driver is the reader thread's caller")
)]
mod notice;
mod team_file;
mod toml_merge;

use std::collections::BTreeSet;
use std::path::PathBuf;

pub use account::{
    MemoryTokenStore, ServerUrl, ServerUrlError, TeamAccount, TeamToken, TokenStore,
};
pub use api::{ApiError, PotluckClient, PotluckInfo, RemoteWorkspace, SignIn, public_url_of};
pub use conflicts::ConflictStore;
pub use flows::{
    Credential, FolderSync, FolderSyncError, JoinError, LoginError, LogoutError, LogoutReport,
    RefreshError, Resolution, TeamResolveError, WorkspacesError, refresh_role, team_join,
    team_login, team_logout, team_resolve, team_sync, team_workspaces,
};
pub use machine::{SyncState, SyncStatus};
pub use team_file::{AccountRecord, BindingRecord, TeamData, TeamFile, TeamFileError};
pub(crate) use toml_merge::merge_workspace_text;

use crate::git::run::{self, Reach};
use crate::git::{
    ConflictFile, GitOptions, Pulled, PushOutcome, Refusal, SyncError, ensure_on_branch, fetch,
    pull_rebase, push, repo_lock, squash_unpushed, unpushed,
};
use crate::paths::StatePaths;

/// The first push plus three retries after the server's branch moved.
const MAX_PUSHES: u32 = 4;

/// Which workspace folder syncs with which server, team, workspace and branch, as whom.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Binding {
    /// The workspace folder, a clone of the team repository.
    pub dir: PathBuf,
    /// The server the account belongs to.
    pub server: String,
    /// The URL git talks to; the token is only ever sent to it.
    pub public_url: String,
    /// The account name, also the commit identity when git has none configured.
    pub username: String,
    /// The team's name on the server.
    pub team: String,
    /// The workspace's name within the team.
    pub workspace: String,
    /// The one branch this folder syncs.
    pub branch: String,
    /// What the account may do in this workspace.
    pub role: Role,
}

impl Binding {
    /// `dir` synced on `branch` at `public_url`, which is also the server, as user `dbento`
    /// with write access and no team or workspace name.
    pub fn new(
        dir: impl Into<PathBuf>,
        public_url: impl Into<String>,
        branch: impl Into<String>,
    ) -> Binding {
        let public_url = public_url.into();
        Binding {
            dir: dir.into(),
            server: public_url.clone(),
            public_url,
            username: "dbento".to_owned(),
            team: String::new(),
            workspace: String::new(),
            branch: branch.into(),
            role: Role::Write,
        }
    }

    /// The same binding for account `username` on `server`.
    pub fn account(mut self, server: impl Into<String>, username: impl Into<String>) -> Binding {
        self.server = server.into();
        self.username = username.into();
        self
    }

    /// The same binding naming its team and workspace.
    pub fn in_workspace(
        mut self,
        team: impl Into<String>,
        workspace: impl Into<String>,
    ) -> Binding {
        self.team = team.into();
        self.workspace = workspace.into();
        self
    }

    /// The same binding with `role`.
    pub fn with_role(mut self, role: Role) -> Binding {
        self.role = role;
        self
    }
}

/// An account's access to a team workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum Role {
    /// May pull but never push.
    Read,
    /// May pull and push.
    Write,
}

/// How one sync cycle ended.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum SyncOutcome {
    /// Local saves reached the server and the server's commits are local.
    Synced,
    /// The server's side won these paths; the local sides are kept in the conflict store. Every
    /// other change was synced.
    Conflicted(Vec<ConflictFile>),
    /// The server refused the push; the saves stay local, folded into one commit.
    Refused(Refusal),
    /// A read-only member followed the server and pushed nothing.
    ReadOnly,
    /// A read-only member did not follow the server, so local work is not overwritten.
    ReadOnlyBlocked(ReadBlock),
}

/// Why a read-only member's folder did not follow the server.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum ReadBlock {
    /// Local edits on paths the server changed, sorted.
    Edits(Vec<String>),
    /// Commits made before the demotion.
    Commits(u32),
}

impl std::fmt::Display for ReadBlock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ReadBlock::Edits(paths) => {
                write!(f, "local edits block updates ({})", paths.join(", "))
            }
            ReadBlock::Commits(1) => f.write_str("1 local commit blocks updates"),
            ReadBlock::Commits(n) => write!(f, "{n} local commits block updates"),
        }
    }
}

/// One locked cycle on `binding.dir`: commit pending changes, pull with rebase, squash the
/// unpushed saves and push, retrying when the server moved meanwhile. `base` carries the
/// caller's options and environment; the token is added here and sent to `public_url` only.
pub fn sync_once(
    binding: &Binding,
    token: &str,
    paths: &StatePaths,
    base: &GitOptions,
) -> Result<SyncOutcome, SyncError> {
    let opts = base.clone().team_env(&binding.public_url, token);
    let _lock = repo_lock(&binding.dir, &opts)?;
    ensure_on_branch(binding, &opts)?;
    match binding.role {
        Role::Write => {}
        Role::Read => return follow(binding, &opts),
    }
    let store = ConflictStore::for_workspace(paths, &binding.dir)?;
    let mut conflicts = Vec::new();
    for _ in 0..MAX_PUSHES {
        match pull_rebase(binding, &store, &opts)? {
            Pulled::Conflicted(files) => conflicts.extend(files),
            Pulled::UpToDate | Pulled::FastForwarded | Pulled::Rebased => {}
        }
        // Squash before every push so a refused blob is never offered again.
        squash_unpushed(binding, &opts)?;
        if unpushed(binding, &opts)? == 0 {
            return Ok(settled(conflicts));
        }
        match push(binding, &opts)? {
            PushOutcome::Pushed => return Ok(settled(conflicts)),
            PushOutcome::NonFastForward => {}
            PushOutcome::Refused(refusal) => return Ok(SyncOutcome::Refused(refusal)),
        }
    }
    Err(SyncError::Raced { pushes: MAX_PUSHES })
}

/// A reader's cycle: never commit or push, only fast-forward to the server, and hold back
/// whenever that would bury commits from before a demotion or overwrite a local edit.
fn follow(binding: &Binding, opts: &GitOptions) -> Result<SyncOutcome, SyncError> {
    fetch(binding, opts)?;
    let target = format!("refs/remotes/origin/{}", binding.branch);
    let behind = format!("HEAD..{target}");
    let count = git_text(&["rev-list", "--count", behind.as_str()], binding, opts)?;
    if count.trim() == "0" {
        return Ok(SyncOutcome::ReadOnly);
    }
    let commits = unpushed(binding, opts)?;
    if commits > 0 {
        return Ok(SyncOutcome::ReadOnlyBlocked(ReadBlock::Commits(commits)));
    }
    // Tracked changes and untracked files both stand in the way of a path the server changed.
    let mut dirty = paths_of(
        &["diff", "--no-renames", "--name-only", "-z", "HEAD"],
        binding,
        opts,
    )?;
    let untracked = ["ls-files", "--others", "--exclude-standard", "-z"];
    dirty.extend(paths_of(&untracked, binding, opts)?);
    let incoming = paths_of(
        &[
            "diff",
            "--no-renames",
            "--name-only",
            "-z",
            "HEAD",
            target.as_str(),
        ],
        binding,
        opts,
    )?;
    let blocked: Vec<String> = dirty.intersection(&incoming).cloned().collect();
    if !blocked.is_empty() {
        return Ok(SyncOutcome::ReadOnlyBlocked(ReadBlock::Edits(blocked)));
    }
    run::git_ok(
        &["merge", "-q", "--ff-only", target.as_str()],
        &binding.dir,
        Reach::Local,
        opts,
    )?;
    Ok(SyncOutcome::ReadOnly)
}

/// The stdout of a local git call in the folder, lossily decoded.
fn git_text(args: &[&str], binding: &Binding, opts: &GitOptions) -> Result<String, SyncError> {
    let out = run::git_ok(args, &binding.dir, Reach::Local, opts)?;
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// The NUL-separated paths a local git call prints, sorted.
fn paths_of(
    args: &[&str],
    binding: &Binding,
    opts: &GitOptions,
) -> Result<BTreeSet<String>, SyncError> {
    let text = git_text(args, binding, opts)?;
    Ok(text
        .split('\0')
        .filter(|p| !p.is_empty())
        .map(str::to_owned)
        .collect())
}

/// `Synced` when no path conflicted, else every conflict the cycle met.
fn settled(conflicts: Vec<ConflictFile>) -> SyncOutcome {
    if conflicts.is_empty() {
        SyncOutcome::Synced
    } else {
        SyncOutcome::Conflicted(conflicts)
    }
}
