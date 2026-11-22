//! Team mode in the app: the injected hooks every team surface reads, the view of a sync, and
//! the session that runs the sync of an open bound workspace. Without installed hooks the app
//! shows no team UI at all.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use dbento_core::paths::StatePaths;
use dbento_core::{
    BindingRecord, ConflictStore, GitOptions, SyncConfig, SyncState, SyncStatus, TeamFile,
    TeamSync, TokenStore,
};
use gpui_kit::{App, AppContext as _, Context, Entity, EventEmitter, Global, Subscription, Task};

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

/// What a session tells the window beyond a redraw.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum TeamEvent {
    /// A file left the conflict list, so what is on disk may have changed under open tabs.
    Settled,
}

/// The running sync of one open bound folder, and the last status it reported.
pub struct TeamSession {
    dir: PathBuf,
    record: BindingRecord,
    conflicts: Option<ConflictStore>,
    sync: Option<TeamSync>,
    view: Option<TeamView>,
    revoke_note: Option<&'static str>,
    _pump: Task<()>,
    _quit: Subscription,
}

impl TeamSession {
    fn start(
        hooks: TeamHooks,
        dir: PathBuf,
        record: BindingRecord,
        cx: &mut Context<TeamSession>,
    ) -> TeamSession {
        let (tx, rx) = async_channel::unbounded::<SyncStatus>();
        // A folder that cannot be resolved has no saved copies to show.
        let conflicts = ConflictStore::for_workspace(&hooks.paths, &dir).ok();
        let config = hooks.sync.clone().git(hooks.git.clone());
        // The driver thread only forwards; all state changes happen on the UI thread.
        let started = TeamSync::start_with(
            record.to_binding(&dir),
            Arc::clone(&hooks.tokens),
            &hooks.paths,
            config,
            move |status| {
                let _ = tx.send_blocking(status);
            },
        );
        let (sync, view) = match started {
            Ok(sync) => (Some(sync), None),
            Err(e) => (
                None,
                Some(TeamView {
                    state: SyncState::Failed(e.to_string()),
                    unpushed: 0,
                    last_synced: None,
                    note: None,
                }),
            ),
        };
        let pump = cx.spawn(async move |this, cx| {
            while let Ok(status) = rx.recv().await {
                if this
                    .update(cx, |s, cx| s.show(TeamView::from(status), cx))
                    .is_err()
                {
                    return;
                }
            }
        });
        // At quit the flush runs to its bound inside the hook body, before the process exits.
        let quit = cx.on_app_quit(|s, _| {
            drop(s.sync.take());
            async {}
        });
        TeamSession {
            dir,
            record,
            conflicts,
            sync,
            view,
            revoke_note: None,
            _pump: pump,
            _quit: quit,
        }
    }

    /// The last status the sync reported, if any yet.
    pub fn view(&self) -> Option<&TeamView> {
        self.view.as_ref()
    }

    /// Why the last sign-out may have left its token live on the server, until a sign-in.
    pub fn revoke_note(&self) -> Option<&str> {
        self.revoke_note
    }

    /// Records whether the last sign-out could not revoke its token; a sign-in passes None.
    pub fn set_revoke_note(&mut self, note: Option<&'static str>, cx: &mut Context<Self>) {
        self.revoke_note = note;
        cx.notify();
    }

    /// The bound folder.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The folder's binding as `team.json` held it when the session started.
    pub fn record(&self) -> &BindingRecord {
        &self.record
    }

    /// The text of my saved side of conflicted `rel`, if a copy is kept.
    pub fn saved_copy(&self, rel: &str) -> Option<String> {
        let path = self.conflicts.as_ref()?.copy_path(rel);
        std::fs::read_to_string(path).ok()
    }

    /// Shows `view` as the sync's status, telling the window when a conflict left the list.
    pub fn show(&mut self, view: TeamView, cx: &mut Context<Self>) {
        let now = conflict_paths(Some(&view));
        let settled = conflict_paths(self.view.as_ref())
            .iter()
            .any(|p| !now.contains(p));
        self.view = Some(view);
        if settled {
            cx.emit(TeamEvent::Settled);
        }
        cx.notify();
    }

    /// A file in the folder changed.
    pub fn changed(&self) {
        if let Some(sync) = &self.sync {
            sync.changed();
        }
    }

    /// Retries whatever holds the sync.
    pub fn retry(&self) {
        if let Some(sync) = &self.sync {
            sync.retry();
        }
    }

    /// Settles conflicted `rel` with the local side.
    pub fn keep_mine(&self, rel: &str) {
        if let Some(sync) = &self.sync {
            sync.keep_mine(rel);
        }
    }

    /// Settles conflicted `rel` with the server's side.
    pub fn take_theirs(&self, rel: &str) {
        if let Some(sync) = &self.sync {
            sync.take_theirs(rel);
        }
    }

    /// Hands the sync to a std thread to drop; resolves once that drop (its flush) ends.
    /// `None` when the sync is already stopped or never started.
    pub fn stop(&mut self) -> Option<impl Future<Output = ()> + Send + 'static + use<>> {
        let sync = self.sync.take()?;
        let (done, finished) = async_channel::bounded::<()>(1);
        // TeamSync::drop waits up to its bound, so never on the UI thread. A thread that cannot
        // start drops both inside spawn's error, and the future resolves at once.
        let _ = std::thread::Builder::new()
            .name("dbento-team-stop".into())
            .spawn(move || {
                drop(sync);
                drop(done);
            });
        Some(async move {
            let _ = finished.recv().await;
        })
    }
}

impl EventEmitter<TeamEvent> for TeamSession {}

fn conflict_paths(view: Option<&TeamView>) -> Vec<String> {
    #[allow(
        clippy::wildcard_enum_match_arm,
        reason = "only Conflicted carries files; every other state has none"
    )]
    match view.map(|v| &v.state) {
        Some(SyncState::Conflicted(files)) => files.iter().map(|f| f.path.clone()).collect(),
        _ => Vec::new(),
    }
}

impl Drop for TeamSession {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

/// The session for the bound folder at or above `dir`; None without hooks or a binding.
pub fn session_for(dir: &Path, cx: &mut App) -> Option<Entity<TeamSession>> {
    let hooks = hooks(cx)?;
    // An unreadable team.json is treated as no binding; the CLI reports it.
    let data = TeamFile::new(&hooks.paths.team).load().ok()?;
    let (folder, record) = data.binding_for(dir)?;
    Some(cx.new(|cx| TeamSession::start(hooks, folder, record, cx)))
}
