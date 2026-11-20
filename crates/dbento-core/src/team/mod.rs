//! Team mode: a workspace folder bound to one branch of a Potluck server's repository.

mod conflicts;

use std::path::PathBuf;

pub use conflicts::ConflictStore;

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
        Role::Read => {
            // A reader only follows the server; commits from before a demotion stay put.
            fetch(binding, &opts)?;
            if unpushed(binding, &opts)? == 0 {
                let target = format!("refs/remotes/origin/{}", binding.branch);
                let args = ["merge", "-q", "--ff-only", target.as_str()];
                run::git_ok(&args, &binding.dir, Reach::Local, &opts)?;
            }
            return Ok(SyncOutcome::ReadOnly);
        }
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

/// `Synced` when no path conflicted, else every conflict the cycle met.
fn settled(conflicts: Vec<ConflictFile>) -> SyncOutcome {
    if conflicts.is_empty() {
        SyncOutcome::Synced
    } else {
        SyncOutcome::Conflicted(conflicts)
    }
}
