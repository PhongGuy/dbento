//! The sync driver: one thread per bound folder that carries out what the state machine
//! decides (cycles and polls) and reports each change of status to its viewer.

use std::collections::{BTreeMap, HashMap};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender, TryRecvError};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use super::account::{ServerUrl, TeamAccount, TeamToken, TimedTokenStore, TokenStore};
use super::api::PotluckClient;
use super::conflicts::ConflictStore;
use super::flows::{
    GitFailure, Resolution, TeamResolveError, diagnose, refresh_role, settle_path, stamp_synced,
};
use super::machine::{Action, Cycle, Ended, Input, Machine, SyncState, SyncStatus, Timing};
use super::notice::{Notice, ReaderConfig, spawn_reader};
use super::team_file::TeamFile;
use super::{Binding, Role, SyncOutcome, git_text, sync_steps};
use crate::git::{
    CancelFlag, ConflictFile, ConflictKind, GitError, GitOptions, SAVE_MESSAGE, SyncError,
    commit_pending, ensure_on_branch, git_available_with, remote_tip, repo_lock, unpushed,
};
use crate::paths::StatePaths;

/// The driver's intervals, limits and git options. [`SyncConfig::new`] gives the defaults.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct SyncConfig {
    debounce: Duration,
    poll_every: Duration,
    silence: Duration,
    backoff_min: Duration,
    backoff_max: Duration,
    drop_bound: Duration,
    token_timeout: Duration,
    git: GitOptions,
}

impl Default for SyncConfig {
    fn default() -> SyncConfig {
        SyncConfig {
            debounce: Duration::from_secs(4),
            poll_every: Duration::from_secs(60),
            silence: Duration::from_secs(45),
            backoff_min: Duration::from_secs(1),
            backoff_max: Duration::from_secs(60),
            drop_bound: Duration::from_secs(10),
            // Below the drop bound, so a keychain that never answers cannot outlast a drop.
            token_timeout: Duration::from_secs(5),
            git: GitOptions::new(),
        }
    }
}

impl SyncConfig {
    /// The defaults: 4 s debounce, 60 s poll, 45 s silence, 1-60 s backoff, 10 s drop bound,
    /// 5 s token timeout and [`GitOptions::new`].
    pub fn new() -> SyncConfig {
        SyncConfig::default()
    }

    /// How long the folder must stay quiet after a change before a cycle runs.
    pub fn debounce(mut self, wait: Duration) -> SyncConfig {
        self.debounce = wait;
        self
    }

    /// How often the server is polled when no notice says it moved.
    pub fn poll_every(mut self, every: Duration) -> SyncConfig {
        self.poll_every = every;
        self
    }

    /// How long an open notice stream may stay silent before polls resume.
    pub fn silence(mut self, limit: Duration) -> SyncConfig {
        self.silence = limit;
        self
    }

    /// The shortest and longest wait between notice stream reconnects.
    pub fn backoff(mut self, min: Duration, max: Duration) -> SyncConfig {
        self.backoff_min = min;
        self.backoff_max = max;
        self
    }

    /// How long dropping the handle waits for the last flush before killing git.
    pub fn drop_bound(mut self, limit: Duration) -> SyncConfig {
        self.drop_bound = limit;
        self
    }

    /// How long each token store call may take.
    pub fn token_timeout(mut self, limit: Duration) -> SyncConfig {
        self.token_timeout = limit;
        self
    }

    /// The options every git call starts from.
    pub fn git(mut self, opts: GitOptions) -> SyncConfig {
        self.git = opts;
        self
    }
}

/// Why a sync driver did not start.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum TeamError {
    /// The binding's server is not a usable URL.
    #[error("{0}")]
    BadServer(String),
    /// The driver thread could not be spawned.
    #[error("cannot start the sync thread: {0}")]
    Spawn(String),
}

/// What the handle tells its thread.
enum Msg {
    Changed,
    Flush,
    Retry,
    KeepMine(String),
    TakeTheirs(String),
    /// From the reader thread.
    Notice(Notice),
    Stop,
}

/// A running sync of one bound folder. Dropping it flushes pending changes, waiting at most
/// the drop bound before killing any git still running; it never joins the thread. Dropping it
/// from inside its own `on_status` callback always waits the full bound, so do not.
pub struct TeamSync {
    tx: Sender<Msg>,
    /// Disconnects when the thread ends.
    done: Mutex<Receiver<()>>,
    cancel: CancelFlag,
    drop_bound: Duration,
}

impl std::fmt::Debug for TeamSync {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TeamSync").finish_non_exhaustive()
    }
}

impl TeamSync {
    /// [`TeamSync::start_with`] with [`SyncConfig::new`].
    pub fn start(
        binding: Binding,
        tokens: Arc<dyn TokenStore>,
        paths: &StatePaths,
        on_status: impl Fn(SyncStatus) + Send + 'static,
    ) -> Result<TeamSync, TeamError> {
        TeamSync::start_with(binding, tokens, paths, SyncConfig::new(), on_status)
    }

    /// Starts syncing `binding` on its own thread, which runs a first cycle at once and calls
    /// `on_status` whenever the status changes. Each token store call is bounded by the
    /// config's token timeout.
    pub fn start_with(
        binding: Binding,
        tokens: Arc<dyn TokenStore>,
        paths: &StatePaths,
        config: SyncConfig,
        on_status: impl Fn(SyncStatus) + Send + 'static,
    ) -> Result<TeamSync, TeamError> {
        let server = ServerUrl::parse(&binding.server).map_err(|e| TeamError::BadServer(e.0))?;
        let account = TeamAccount::new(server, binding.username.as_str());
        let file = TeamFile::new(&paths.team);
        let data = file.load().ok();
        let last_synced = data
            .as_ref()
            .and_then(|d| d.binding_for(&binding.dir))
            .and_then(|(_, rec)| rec.last_synced)
            .map(|secs| UNIX_EPOCH + Duration::from_secs(secs));
        let insecure_http = data
            .as_ref()
            .and_then(|d| d.account(&account.server, Some(&account.username)))
            .is_some_and(|rec| rec.insecure_http);
        let cancel = CancelFlag::new();
        let timing = Timing {
            debounce: config.debounce,
            poll_every: config.poll_every,
            silence: config.silence,
        };
        let (tx, rx) = mpsc::channel();
        let driver = Driver {
            machine: Machine::new(binding.role, timing, Instant::now()),
            binding,
            account,
            insecure_http,
            reader_config: ReaderConfig {
                backoff_min: config.backoff_min,
                backoff_max: config.backoff_max,
            },
            reader: None,
            notices: tx.clone(),
            tokens: TimedTokenStore::new(tokens, config.token_timeout),
            paths: paths.clone(),
            file,
            git: config.git.cancel(cancel.clone()),
            kinds: HashMap::new(),
            report: Report {
                on_status: Box::new(on_status),
                last: None,
                unpushed: 0,
                last_synced,
                note: None,
            },
        };
        let (done_tx, done_rx) = mpsc::channel::<()>();
        std::thread::Builder::new()
            .name("dbento-team-sync".to_owned())
            .spawn(move || {
                let _done = done_tx;
                driver.run(&rx);
            })
            .map_err(|e| TeamError::Spawn(e.to_string()))?;
        Ok(TeamSync {
            tx,
            done: Mutex::new(done_rx),
            cancel,
            drop_bound: config.drop_bound,
        })
    }

    /// A file in the folder changed; a cycle runs once the folder has been quiet.
    pub fn changed(&self) {
        let _ = self.tx.send(Msg::Changed);
    }

    /// Sync now, without waiting for the folder to go quiet.
    pub fn flush(&self) {
        let _ = self.tx.send(Msg::Flush);
    }

    /// Retry whatever holds the sync, such as a sign-out or missing git.
    pub fn retry(&self) {
        let _ = self.tx.send(Msg::Retry);
    }

    /// Settle conflicted path `rel` by putting the local side back, then sync it.
    pub fn keep_mine(&self, rel: &str) {
        let _ = self.tx.send(Msg::KeepMine(rel.to_owned()));
    }

    /// Settle conflicted path `rel` by keeping the server's side and dropping the local one.
    pub fn take_theirs(&self, rel: &str) {
        let _ = self.tx.send(Msg::TakeTheirs(rel.to_owned()));
    }
}

impl Drop for TeamSync {
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Stop);
        let done = self.done.get_mut().unwrap_or_else(|e| e.into_inner());
        let _ = done.recv_timeout(self.drop_bound);
        // A flush still running past the bound is killed rather than waited for.
        self.cancel.cancel();
    }
}

/// The status last sent and the parts of it that outlive one state.
struct Report {
    on_status: Box<dyn Fn(SyncStatus) + Send>,
    last: Option<SyncStatus>,
    unpushed: u32,
    last_synced: Option<SystemTime>,
    note: Option<String>,
}

impl Report {
    /// Sends the status in `state`, unless it equals the last one sent.
    fn show(&mut self, state: SyncState) {
        let status = SyncStatus {
            state,
            unpushed: self.unpushed,
            last_synced: self.last_synced,
            note: self.note.clone(),
        };
        if self.last.as_ref() != Some(&status) {
            (self.on_status)(status.clone());
            self.last = Some(status);
        }
    }
}

/// The running reader: dropping `stop` ends it at its next wait. It is never joined.
struct Reader {
    stop: Option<Sender<()>>,
    handle: JoinHandle<()>,
}

/// The thread's side: the machine and everything a cycle or poll needs.
struct Driver {
    machine: Machine,
    binding: Binding,
    account: TeamAccount,
    insecure_http: bool,
    reader_config: ReaderConfig,
    reader: Option<Reader>,
    /// What a new reader sends its notices through.
    notices: Sender<Msg>,
    tokens: TimedTokenStore,
    paths: StatePaths,
    file: TeamFile,
    git: GitOptions,
    /// Each path's kind at the last conflict this thread saw.
    kinds: HashMap<String, ConflictKind>,
    report: Report,
}

impl Driver {
    /// Runs until stopped or the handle is gone. Queued messages are taken before each step,
    /// so a stop is seen between steps and never waits behind a debounce.
    fn run(mut self, rx: &Receiver<Msg>) {
        self.report.show(self.machine.state().clone());
        self.spawn_reader();
        loop {
            loop {
                match rx.try_recv() {
                    Ok(Msg::Stop) => return self.stop(),
                    Ok(msg) => self.take(msg),
                    Err(TryRecvError::Empty) => break,
                    Err(TryRecvError::Disconnected) => return,
                }
            }
            let got = match self.machine.next(Instant::now()) {
                Action::Run(cycle) => {
                    self.cycle(cycle);
                    continue;
                }
                Action::Poll => {
                    let ended = self.poll();
                    self.machine.on(Input::Ended(ended), Instant::now());
                    self.report.show(self.machine.state().clone());
                    continue;
                }
                Action::Wait(Some(at)) => {
                    match rx.recv_timeout(at.saturating_duration_since(Instant::now())) {
                        Ok(msg) => msg,
                        Err(RecvTimeoutError::Timeout) => continue,
                        Err(RecvTimeoutError::Disconnected) => return,
                    }
                }
                Action::Wait(None) => match rx.recv() {
                    Ok(msg) => msg,
                    Err(_) => return,
                },
            };
            match got {
                Msg::Stop => return self.stop(),
                msg @ (Msg::Changed
                | Msg::Flush
                | Msg::Retry
                | Msg::KeepMine(_)
                | Msg::TakeTheirs(_)
                | Msg::Notice(_)) => self.take(msg),
            }
        }
    }

    /// The last flush: at most one cycle, then the thread ends.
    fn stop(mut self) {
        self.machine.on(Input::Flush, Instant::now());
        if let Action::Run(cycle) = self.machine.next(Instant::now()) {
            self.cycle(cycle);
        }
    }

    fn take(&mut self, msg: Msg) {
        let input = match msg {
            Msg::Changed => Input::Changed,
            Msg::Flush => Input::Flush,
            Msg::Retry => {
                if *self.machine.state() == SyncState::SignedOut {
                    self.spawn_reader();
                }
                Input::Retry
            }
            Msg::KeepMine(rel) => return self.settle(&rel, Resolution::KeepMine),
            Msg::TakeTheirs(rel) => return self.settle(&rel, Resolution::TakeTheirs),
            Msg::Notice(notice) => return self.notice(notice),
            Msg::Stop => return,
        };
        self.machine.on(input, Instant::now());
        self.report.show(self.machine.state().clone());
    }

    /// Starts a notice reader, unless one is still running or there is no token. A reader stuck
    /// on a silent socket cannot be stopped, so none is started beside it; polls cover the gap.
    fn spawn_reader(&mut self) {
        if self
            .reader
            .as_ref()
            .is_some_and(|r| !r.handle.is_finished())
        {
            return;
        }
        self.reader = None;
        let Ok(Some(token)) = self.tokens.get(&self.account) else {
            return;
        };
        let Ok(client) = PotluckClient::new(&self.account.server, self.insecure_http) else {
            return;
        };
        let (stop, stop_rx) = mpsc::channel();
        let out = self.notices.clone();
        let spawned = spawn_reader(
            client,
            token,
            &self.binding.team,
            &self.binding.workspace,
            self.reader_config,
            stop_rx,
            move |n| out.send(Msg::Notice(n)).is_ok(),
        );
        if let Ok(handle) = spawned {
            self.reader = Some(Reader {
                stop: Some(stop),
                handle,
            });
        }
    }

    /// Feeds one notice to the machine. An opened stream rechecks the role, and a ref notice
    /// counts only for the bound branch at a tip other than the one last fetched.
    fn notice(&mut self, notice: Notice) {
        let now = Instant::now();
        match notice {
            Notice::Connected => {
                self.machine.on(Input::StreamUp, now);
                if let Ok(rec) = refresh_role(&self.tokens, &self.paths, &self.binding.dir) {
                    let role = rec.to_binding(&self.binding.dir).role;
                    if role != self.binding.role {
                        self.binding.role = role;
                        self.machine.on(Input::Role(role), now);
                    }
                }
            }
            Notice::Alive => self.machine.on(Input::StreamAlive, now),
            Notice::Ref { git_ref, sha } => {
                self.machine.on(Input::StreamAlive, now);
                let ours = git_ref == format!("refs/heads/{}", self.binding.branch);
                if ours && self.tracked_tip().as_deref() != Some(sha.as_str()) {
                    self.machine.on(Input::RemoteMoved, now);
                }
            }
            Notice::Down => self.machine.on(Input::StreamDown, now),
            Notice::SignedOut => self.machine.on(Input::SignedOut, now),
        }
        self.report.show(self.machine.state().clone());
    }

    /// Where the bound branch was at the last fetch.
    fn tracked_tip(&self) -> Option<String> {
        let tracked = format!("refs/remotes/origin/{}", self.binding.branch);
        git_text(
            &["rev-parse", "-q", "--verify", tracked.as_str()],
            &self.binding,
            &self.git,
        )
        .ok()
        .map(|s| s.trim().to_owned())
    }

    /// Asks the server why git failed: a rejected token signs out, a lost write role demotes
    /// the folder with its commits kept, and lost access or anything unexplained fails.
    fn diagnosed(&mut self, e: SyncError, token: &TeamToken) -> Ended {
        let SyncError::Git(git) = e else {
            return ended_by(e);
        };
        if let GitError::Missing = git {
            return Ended::GitMissing;
        }
        let failure = PotluckClient::new(&self.account.server, self.insecure_http)
            .map_or(GitFailure::Other, |client| {
                diagnose(&self.tokens, &self.paths, &self.binding.dir, &client, token)
            });
        match failure {
            GitFailure::SignedOut => {
                // The reader holds the refused token; once it ends, a retry starts a fresh one.
                if let Some(reader) = &mut self.reader {
                    reader.stop = None;
                }
                Ended::SignedOut
            }
            // Only a folder that could write is demoted; a reader's own failure stays an error.
            GitFailure::Demoted(_) if self.binding.role == Role::Write => {
                self.binding.role = Role::Read;
                self.machine.on(Input::Role(Role::Read), Instant::now());
                Ended::ReadOnly
            }
            GitFailure::NoAccess => Ended::Failed(
                "you no longer have access to this workspace on the server".to_owned(),
            ),
            GitFailure::Offline => Ended::Offline,
            GitFailure::Demoted(_) | GitFailure::Other => {
                Ended::Failed(SyncError::Git(git).to_string())
            }
        }
    }

    /// Settles one conflicted path under the repo lock. A held lock reports busy and writes
    /// nothing; keeping mine then syncs the restored side at once.
    fn settle(&mut self, rel: &str, choice: Resolution) {
        let now = Instant::now();
        match settle_path(&self.paths, &self.binding.dir, rel, choice, &self.git) {
            Ok(()) | Err(TeamResolveError::NoSavedCopy(_)) => match self.outstanding() {
                Ok(left) => {
                    self.machine.on(Input::Settled(left), now);
                    if let Resolution::KeepMine = choice {
                        self.machine.on(Input::Flush, now);
                    }
                }
                Err(why) => self.machine.on(Input::Ended(Ended::Failed(why)), now),
            },
            Err(TeamResolveError::Busy) => self.machine.on(Input::Ended(Ended::Busy), now),
            Err(e) => self
                .machine
                .on(Input::Ended(Ended::Failed(e.to_string())), now),
        }
        self.report.show(self.machine.state().clone());
    }

    /// The conflicts still waiting in the store, so they outlive the thread. A path that is
    /// both copied and marked was last a local delete, so its mark wins. A store that cannot be
    /// read is an error, never "no conflicts".
    fn outstanding(&self) -> Result<Vec<ConflictFile>, String> {
        let unreadable = |e: std::io::Error| format!("cannot read the conflict copies: {e}");
        let store =
            ConflictStore::for_workspace(&self.paths, &self.binding.dir).map_err(unreadable)?;
        // Sorted by path, with marks written last so they replace a copy.
        let mut left = BTreeMap::new();
        for p in store.copies().map_err(unreadable)? {
            left.insert(p, ConflictKind::BothChanged);
        }
        for p in store.deleted().map_err(unreadable)? {
            left.insert(p, ConflictKind::DeletedLocally);
        }
        Ok(left
            .into_iter()
            .map(|(path, fallback)| {
                let kind = self.kinds.get(&path).copied().unwrap_or(fallback);
                ConflictFile { path, kind }
            })
            .collect())
    }

    /// Carries out one cycle, then recounts the unpushed commits and reports how it ended.
    fn cycle(&mut self, cycle: Cycle) {
        let (ended, outcome) = self.carry_out(cycle);
        self.report.note = if let Some(SyncOutcome::ReadOnlyBlocked(block)) = &outcome {
            Some(block.to_string())
        } else {
            None
        };
        if matches!(outcome, Some(SyncOutcome::Synced | SyncOutcome::ReadOnly)) {
            self.stamp();
        }
        if let Ok(n) = unpushed(&self.binding, &self.git) {
            self.report.unpushed = n;
        }
        // Conflicts left from an earlier cycle or run still need a choice.
        let ended = if matches!(
            ended,
            Ended::Synced | Ended::ReadOnly | Ended::Conflicted(_)
        ) {
            match self.outstanding() {
                Ok(left) if left.is_empty() => ended,
                Ok(left) => Ended::Conflicted(left),
                Err(why) => Ended::Failed(why),
            }
        } else {
            ended
        };
        self.machine.on(Input::Ended(ended), Instant::now());
        self.report.show(self.machine.state().clone());
    }

    /// How the cycle ended, and the git outcome when one ran to the end.
    fn carry_out(&mut self, cycle: Cycle) -> (Ended, Option<SyncOutcome>) {
        if let Err(GitError::Missing) = git_available_with(&self.git) {
            return (Ended::GitMissing, None);
        }
        let binding = match cycle {
            Cycle::CommitOnly => {
                self.report.show(SyncState::Committing);
                return match self.commit_only() {
                    Ok(()) => (Ended::SignedOut, None),
                    Err(e) => (ended_by(e), None),
                };
            }
            Cycle::PullOnly => self.binding.clone().with_role(Role::Read),
            Cycle::Sync => self.binding.clone(),
        };
        let token = match self.tokens.get(&self.account) {
            Ok(Some(token)) => token,
            Ok(None) => return (Ended::SignedOut, None),
            Err(e) => return (Ended::Failed(e.to_string()), None),
        };
        let report = &mut self.report;
        let got = sync_steps(&binding, token.expose(), &self.paths, &self.git, &mut |s| {
            report.show(s);
        });
        match got {
            Ok(outcome) => {
                let ended = match &outcome {
                    SyncOutcome::Synced => Ended::Synced,
                    SyncOutcome::ReadOnly | SyncOutcome::ReadOnlyBlocked(_) => Ended::ReadOnly,
                    SyncOutcome::Conflicted(files) => {
                        for f in files {
                            self.kinds.insert(f.path.clone(), f.kind);
                        }
                        Ended::Conflicted(files.clone())
                    }
                    SyncOutcome::Refused(refusal) => Ended::Refused(refusal.clone()),
                };
                (ended, Some(outcome))
            }
            Err(e) => (self.diagnosed(e, &token), None),
        }
    }

    /// Signed out, local changes are still committed, under the lock, without the server.
    fn commit_only(&self) -> Result<(), SyncError> {
        let _lock = repo_lock(&self.binding.dir, &self.git)?;
        ensure_on_branch(&self.binding, &self.git)?;
        commit_pending(&self.binding, SAVE_MESSAGE, &self.git)?;
        Ok(())
    }

    /// Records the sync in the team file, keeping the time shown when that fails.
    fn stamp(&mut self) {
        let record = self
            .file
            .load()
            .ok()
            .and_then(|d| d.binding_for(&self.binding.dir));
        if let Some((folder, record)) = record
            && let Ok(stamped) = stamp_synced(&self.file, &folder, record)
        {
            self.report.last_synced = stamped
                .last_synced
                .map(|secs| UNIX_EPOCH + Duration::from_secs(secs));
        }
    }

    /// Asks the server where the bound branch is; a tip other than the one last fetched means
    /// a cycle should pull it.
    fn poll(&mut self) -> Ended {
        let token = match self.tokens.get(&self.account) {
            Ok(Some(token)) => token,
            Ok(None) => return Ended::SignedOut,
            Err(e) => return Ended::Failed(e.to_string()),
        };
        let opts = self
            .git
            .clone()
            .team_env(&self.binding.public_url, token.expose());
        match remote_tip(&self.binding, &opts) {
            Ok(Some(tip)) => {
                if self.tracked_tip().as_deref() != Some(tip.as_str()) {
                    self.machine.on(Input::RemoteMoved, Instant::now());
                }
                Ended::PollSame
            }
            Ok(None) => Ended::Failed(
                SyncError::BranchGone {
                    branch: self.binding.branch.clone(),
                }
                .to_string(),
            ),
            Err(e) => self.diagnosed(e, &token),
        }
    }
}

/// How a cycle that failed with `e` ends: a held lock or a racing server is worth a retry, and
/// a git that is gone waits for one.
fn ended_by(e: SyncError) -> Ended {
    match e {
        SyncError::Git(GitError::Missing) => Ended::GitMissing,
        SyncError::Busy { .. } | SyncError::Raced { .. } => Ended::Busy,
        other @ (SyncError::Git(_)
        | SyncError::WrongBranch { .. }
        | SyncError::Diverged { .. }
        | SyncError::BranchGone { .. }
        | SyncError::Io(_)) => Ended::Failed(other.to_string()),
    }
}
