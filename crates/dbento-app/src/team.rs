//! Team mode in the app: the injected hooks every team surface reads, and the view of a sync.
//! Without installed hooks the app shows no team UI at all.

use std::sync::Arc;
use std::time::SystemTime;

use dbento_core::paths::StatePaths;
use dbento_core::{GitOptions, SyncConfig, SyncState, SyncStatus, TokenStore};
use gpui_kit::{App, Global};

/// What team mode runs on: the token store, the state files, and how git and the sync run.
/// `main.rs` installs the real ones; tests install a memory store, a temp state dir and a
/// scratch git environment.
#[derive(Clone)]
pub struct TeamHooks {
    tokens: Arc<dyn TokenStore>,
    paths: StatePaths,
    git: GitOptions,
    sync: SyncConfig,
}

impl TeamHooks {
    /// Hooks over `tokens` and `paths`, with default git options and sync timing.
    pub fn new(tokens: Arc<dyn TokenStore>, paths: StatePaths) -> TeamHooks {
        TeamHooks {
            tokens,
            paths,
            git: GitOptions::new(),
            sync: SyncConfig::new(),
        }
    }

    /// The same hooks running git with `opts`.
    pub fn git(self, opts: GitOptions) -> TeamHooks {
        TeamHooks { git: opts, ..self }
    }

    /// The same hooks syncing with `config`.
    pub fn sync(self, config: SyncConfig) -> TeamHooks {
        TeamHooks {
            sync: config,
            ..self
        }
    }

    /// Where session tokens are kept.
    pub fn tokens(&self) -> &Arc<dyn TokenStore> {
        &self.tokens
    }

    /// The state files, `team.json` and the conflict copies among them.
    pub fn paths(&self) -> &StatePaths {
        &self.paths
    }

    /// How git is run.
    pub fn git_options(&self) -> &GitOptions {
        &self.git
    }

    /// The sync's intervals and limits.
    pub fn sync_config(&self) -> &SyncConfig {
        &self.sync
    }
}

struct Installed(TeamHooks);
impl Global for Installed {}

/// Installs the hooks as an app global; a second call keeps the first.
pub fn install(hooks: TeamHooks, cx: &mut App) {
    if cx.try_global::<Installed>().is_none() {
        cx.set_global(Installed(hooks));
    }
}

/// The installed hooks, if any.
pub fn hooks(cx: &App) -> Option<TeamHooks> {
    cx.try_global::<Installed>().map(|i| i.0.clone())
}

/// What the app shows of a sync: the core status, owned by the app so tests can build one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TeamView {
    /// Where the sync stands.
    pub state: SyncState,
    /// Local commits not yet on the server.
    pub unpushed: u32,
    /// When the folder last matched the server.
    pub last_synced: Option<SystemTime>,
    /// Why a read-only folder is not following the server, when it is not.
    pub note: Option<String>,
}

impl From<SyncStatus> for TeamView {
    fn from(s: SyncStatus) -> TeamView {
        TeamView {
            state: s.state,
            unpushed: s.unpushed,
            last_synced: s.last_synced,
            note: s.note,
        }
    }
}
