//! Team mode: a workspace folder bound to one branch of a Potluck server's repository.

use std::path::PathBuf;

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
