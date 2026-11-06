//! One query tab's database session, owned by a tokio task. The view sends `Command`s and
//! reads `Event`s, so it never awaits the database itself.

use std::future::Future;
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_channel::{Receiver, Sender, TryRecvError};
use dbento_core::{
    CancelHandle, ColumnInfo, ConnectionConfig, EditTarget, Engine, ExecuteError, ExplainMode,
    ExplainOutput, ExplainRequest, ForeignKey, Limits, OnTruncate, Outcome, ReadOnlyEnforcement,
    RelationRef, ResultBatch, ResultStream, Secret, SecretError, SecretStore, Session,
    SessionOptions, TruncatedBy, Unguarded, Workspace, Writes, connect, resolve_key_passphrase,
    resolve_secret, secret_hint,
};
use futures_util::FutureExt as _;
use futures_util::future::{Either, select};

use crate::app::EnvLookup;
use crate::parks::{self, Parked};

/// The answer to `Command::SourceColumns`: per column, the relation's column it reads.
pub type SourceColumns = Result<Vec<Option<String>>, String>;

/// What the view asks of its runner.
#[derive(Debug)]
pub enum Command {
    /// Run every statement of `sql` in order.
    Run {
        run_id: u64,
        sql: String,
        limits: AppLimits,
    },
    /// Explain one checked statement; an ANALYZE that could write asks first on a writable
    /// session. Ends with `Planned` and `RunDone`, or a skipped `StatementDone` on Skip or Stop.
    Explain {
        run_id: u64,
        request: ExplainRequest,
    },
    /// Run the statement that is waiting on a confirmation.
    Confirm { run_id: u64, stmt: usize },
    /// Leave the waiting statement unrun.
    Skip { run_id: u64, stmt: usize },
    /// Send one more batch of the parked result of run `run_id`.
    Pull { run_id: u64 },
    /// Continue run `run_id`'s last statement past its cap by `more` rows, or to its end on
    /// `None`; ignored unless that statement is parked at its cap.
    Fetch { run_id: u64, more: Option<u64> },
    /// End run `run_id`: its current stream stops and no later statement starts.
    Stop { run_id: u64 },
    /// Close the session and end the task.
    Close,
    /// Open the session now if none is open, so the next `Run` does not wait for it.
    Connect,
    /// Describe `rel` for editing on this tab's session, connecting first. Sent only while no
    /// run is active; a run in progress drops it.
    Target { rel: RelationRef },
    /// Name the column of `rel` each of `columns` reads (`Session::source_columns`) and send
    /// the answer on `reply`. Sent only while no run is active; a run in progress drops it.
    SourceColumns {
        rel: RelationRef,
        columns: Arc<[ColumnInfo]>,
        reply: Sender<SourceColumns>,
    },
}

/// What the runner reports, in order. Every `Run` ends with `RunDone`; `Stopped` means the
/// task panicked and the tab needs a fresh runner.
#[derive(Debug)]
pub enum Event {
    Connected {
        read_only: bool,
        /// How the session refuses writes; None when it is not read-only.
        level: Option<ReadOnlyEnforcement>,
        engine: Engine,
        cancel: CancelHandle,
    },
    ConnectFailed {
        message: String,
    },
    /// Sent as a statement begins executing, before its columns are known: SQLite reports
    /// them only once its first step ends.
    Executing {
        run_id: u64,
        stmt: usize,
        sql: String,
    },
    StatementStarted {
        run_id: u64,
        stmt: usize,
        sql: String,
        columns: Arc<[ColumnInfo]>,
    },
    NeedsConfirm {
        run_id: u64,
        stmt: usize,
        sql: String,
        kinds: Vec<Unguarded>,
        /// The question is for an explain's ANALYZE, which runs the statement and rolls it back.
        analyze: bool,
    },
    /// An explain's answer; `elapsed` excludes the wait for a confirmation.
    Planned {
        run_id: u64,
        result: Result<ExplainOutput, ExecuteError>,
        elapsed: Duration,
    },
    Batch {
        run_id: u64,
        stmt: usize,
        batch: ResultBatch,
    },
    /// The last statement's stream has parked for the first time; each `Pull` sends one
    /// more batch.
    Paused {
        run_id: u64,
        stmt: usize,
    },
    /// The last statement parked at its row cap; `elapsed` is its active time so far.
    ParkedAtCap {
        run_id: u64,
        stmt: usize,
        outcome: Outcome,
        elapsed: Duration,
    },
    /// The park ended without a Fetch (`statement_timeout` false) or the statement timeout
    /// fired after it (true); a `Truncated` StatementDone follows.
    ParkExpired {
        run_id: u64,
        statement_timeout: bool,
    },
    /// The park at the cap ended so a commit to the same SQLite file could take its lock; a
    /// `Truncated` StatementDone follows.
    ParkReleased {
        run_id: u64,
    },
    StatementDone {
        run_id: u64,
        stmt: usize,
        end: StatementEnd,
        elapsed: Duration,
    },
    RunDone {
        run_id: u64,
    },
    Stopped {
        message: String,
    },
    /// Sent before `RunDone` once a run has left the session's settings changed, on any
    /// engine; the mark lasts until the session closes.
    SettingsChanged,
    /// Sent instead of any statement event, then `RunDone`, for the first `Run` or `Explain`
    /// after the runner reopened a session whose USE/SET changes were lost.
    SettingsLost {
        run_id: u64,
    },
    /// The session ended; the next `Run` reconnects.
    Disconnected {
        reason: String,
    },
    /// The answer to `Command::Target`; a failed connect or catalog read is `Err(message)`.
    Target {
        rel: RelationRef,
        result: Result<EditTarget, String>,
        /// The relation's keys in both directions; empty when the target failed or they could not be read.
        foreign_keys: Vec<ForeignKey>,
    },
    /// A drain is discarding the unread rows of run `run_id`.
    Settling {
        run_id: u64,
    },
    /// Settling after a run reported an error; the session is kept.
    SettleFailed {
        message: String,
    },
}

/// How one statement ended.
#[derive(Debug)]
pub enum StatementEnd {
    Complete(Outcome),
    Truncated(Outcome),
    Skipped,
    Failed(ExecuteError),
    /// Cut short after sending `rows` rows; a stream that fails ends `Failed` instead.
    Stopped {
        rows: u64,
        reason: StopReason,
    },
    /// An explain that ended with output; its plan result holds it.
    Planned,
}

/// Why a statement's stream was cut short.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// A `Stop` of its run.
    User,
    /// A new `Run` replaced it.
    NewRun,
    /// A `Close` ended the session.
    Closed,
    /// The runner itself died.
    Lost,
    /// The next batch would have taken the result past its memory budget.
    MemoryBudget,
}

/// The batch heap of 1M `wide` rows, the larger of the two engines' measured `load_wide_*` figures.
pub const WIDE_1M_HEAP_BYTES: u64 = 561_065_127;

/// The default memory budget per result: 1.25 x [`WIDE_1M_HEAP_BYTES`], rounded up to a
/// multiple of 64 MiB (704 MiB).
pub const DEFAULT_MAX_RESULT_BYTES: u64 = 11 * (64 << 20);

/// The app's per-statement limits; the default caps a result at 100,000 rows and at
/// [`DEFAULT_MAX_RESULT_BYTES`] of batch heap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AppLimits {
    pub max_rows: Option<u64>,
    /// The batch heap a result may hold; the batch that would pass it ends the run.
    pub max_result_bytes: u64,
}

impl Default for AppLimits {
    fn default() -> Self {
        AppLimits {
            max_rows: Some(1_000),
            max_result_bytes: DEFAULT_MAX_RESULT_BYTES,
        }
    }
}

/// How eagerly a runner streams the last statement's result.
#[derive(Debug, Clone, Copy)]
pub struct RunnerTuning {
    /// Rows sent before the stream may park; one more batch follows them.
    pub first_rows: u64,
    /// How long a parked stream waits for a `Pull` before it is pulled to its end or cap.
    pub idle_complete: Duration,
    /// The capacity of the event channel.
    pub events_capacity: usize,
    /// How long a stream parked at its cap waits for a Fetch.
    pub park_at_cap: Duration,
}

/// How long a stream parked at its cap waits for a Fetch by default.
pub const PARK_AT_CAP: Duration = Duration::from_secs(60);

impl Default for RunnerTuning {
    fn default() -> Self {
        RunnerTuning {
            first_rows: 200,
            idle_complete: Duration::from_secs(15),
            events_capacity: 64,
            park_at_cap: PARK_AT_CAP,
        }
    }
}

/// Everything a runner needs to open its session.
#[derive(Clone)]
pub struct RunnerSpec {
    pub workspace: Arc<Workspace>,
    pub connection: String,
    pub store: Arc<dyn SecretStore>,
    pub env: EnvLookup,
}

/// The runner's session, shared with [`guarded`] so a panic can still close it.
pub type SessionSlot = Arc<tokio::sync::Mutex<Option<Session>>>;

/// The view's end of a runner. Dropping it closes the session, as `Close` does.
pub struct Runner {
    commands: Sender<Command>,
    events: Receiver<Event>,
}

impl Runner {
    /// Queues `command`; the inbox is unbounded, so only a runner that has ended drops it.
    pub fn send(&self, command: Command) {
        let _ = self.commands.try_send(command);
    }

    /// The event stream; it closes when the runner ends.
    pub fn events(&self) -> Receiver<Event> {
        self.events.clone()
    }
}

/// Starts a runner on `handle`. The task owns the session and ends once `Close` arrives or
/// the `Runner` is dropped.
pub fn spawn_on(handle: &tokio::runtime::Handle, spec: RunnerSpec) -> Runner {
    spawn_tuned(handle, spec, RunnerTuning::default())
}

/// [`spawn_on`] with explicit streaming tuning.
pub fn spawn_tuned(
    handle: &tokio::runtime::Handle,
    spec: RunnerSpec,
    tuning: RunnerTuning,
) -> Runner {
    // Commands come from user actions and Pulls are throttled, so the inbox cannot run away.
    let (commands, inbox) = async_channel::unbounded();
    let (outbox, events) = async_channel::bounded(tuning.events_capacity);
    let slot = SessionSlot::default();
    let body = run(spec, tuning, inbox, outbox.clone(), slot.clone());
    handle.spawn(guarded(body, slot, outbox));
    Runner { commands, events }
}

/// Runs `body`; on a panic, closes whatever session `slot` holds and sends `Stopped`, so the
/// view never waits on a dead task and no session is left open.
pub async fn guarded(body: impl Future<Output = ()>, slot: SessionSlot, events: Sender<Event>) {
    // The body, and with it any guard it held on the slot, is gone once the await ends.
    let result = AssertUnwindSafe(body).catch_unwind().await;
    if result.is_err() {
        let held = slot.lock().await.take();
        if let Some(s) = held {
            let _ = s.close().await;
        }
        // Waits for room rather than dropping the one event the view must see.
        let _ = events
            .send(Event::Stopped {
                message: "session stopped".into(),
            })
            .await;
    }
}

async fn run(
    spec: RunnerSpec,
    tuning: RunnerTuning,
    inbox: Receiver<Command>,
    events: Sender<Event>,
    slot: SessionSlot,
) {
    // Held for the task's whole life; a panic drops it while unwinding, freeing `guarded`.
    let mut session = slot.lock().await;
    // Set when the runner drops a session whose USE/SET changes a reopen would lose.
    let mut lost_settings = false;
    let mut next = idle(&inbox, &mut session, &mut lost_settings, &events).await;
    while let Some(command) = next.take() {
        if matches!(command, Command::Connect) {
            ensure_open(&spec, &mut session, &events).await;
            next = idle(&inbox, &mut session, &mut lost_settings, &events).await;
            continue;
        }
        if let Command::Target { rel } = command {
            // Every Target is answered, so a failed connect still ends the tab's wait.
            ensure_open(&spec, &mut session, &events).await;
            let (result, foreign_keys) = match session.as_mut() {
                // An engine core cannot edit yet answers before edit_target, so the tab browses.
                Some(s) if !s.capabilities().edit_rows => (
                    Err(format!(
                        "editing rows is not available on {} yet",
                        s.engine().name()
                    )),
                    Vec::new(),
                ),
                Some(s) => match s.edit_target(&rel).await {
                    // Keys are a nicety: a failed read is logged and the table browses without them.
                    Ok(t) => {
                        let keys = s.foreign_keys(&rel).await.unwrap_or_else(|e| {
                            log::debug!("foreign keys of {rel:?}: {e}");
                            Vec::new()
                        });
                        (Ok(t), keys)
                    }
                    Err(e) => (Err(e.to_string()), Vec::new()),
                },
                None => (Err("not connected".to_owned()), Vec::new()),
            };
            let _ = events
                .send(Event::Target {
                    rel,
                    result,
                    foreign_keys,
                })
                .await;
            next = idle(&inbox, &mut session, &mut lost_settings, &events).await;
            continue;
        }
        if let Command::SourceColumns {
            rel,
            columns,
            reply,
        } = command
        {
            ensure_open(&spec, &mut session, &events).await;
            let result = match session.as_mut() {
                Some(s) => s
                    .source_columns(&rel, &columns)
                    .await
                    .map_err(|e| e.to_string()),
                None => Err("not connected".to_owned()),
            };
            let _ = reply.try_send(result);
            next = idle(&inbox, &mut session, &mut lost_settings, &events).await;
            continue;
        }
        let (run_id, work) = match command {
            Command::Run {
                run_id,
                sql,
                limits,
            } => (run_id, Work::Script(sql, limits)),
            Command::Explain { run_id, request } => (run_id, Work::Explain(request)),
            Command::Close => break,
            _ => {
                next = idle(&inbox, &mut session, &mut lost_settings, &events).await; // a Confirm, Skip, Pull or Fetch nobody waits for
                continue;
            }
        };
        // A reopened session would run on the configured database, so the first run after
        // losing a switch is refused once and the next one proceeds.
        if std::mem::take(&mut lost_settings) {
            let _ = events.send(Event::SettingsLost { run_id }).await;
            let _ = events.send(Event::RunDone { run_id }).await;
            next = idle(&inbox, &mut session, &mut lost_settings, &events).await;
            continue;
        }
        // A Run connects if no pre-connect did, and again after a failure, so it retries.
        ensure_open(&spec, &mut session, &events).await;
        let cx = Ctx {
            run_id,
            inbox: &inbox,
            events: &events,
            tuning,
            file: session
                .as_ref()
                .filter(|s| s.engine() == Engine::Sqlite)
                .and_then(|_| sqlite_file(&spec)),
        };
        let mut settings_changed = false;
        let after = match session.as_mut() {
            Some(s) => {
                // An explain settles its own block, so it leaves the session clean.
                let (after, tail) = match &work {
                    Work::Script(sql, limits) => run_script(s, sql, *limits, &cx).await,
                    Work::Explain(request) => (run_explain(s, request, &cx).await, Tail::Clean),
                };
                settings_changed = s.settings_changed();
                // A Close or a hangup closes the session next anyway, and core's close
                // drains a write's rows itself so its commit is kept.
                if matches!(
                    after,
                    After::Idle | After::Next(Command::Run { .. } | Command::Explain { .. })
                ) {
                    finish(&mut session, &mut lost_settings, tail, run_id, &events).await;
                }
                after
            }
            None => After::Idle,
        };
        if settings_changed {
            let _ = events.send(Event::SettingsChanged).await;
        }
        let _ = events.send(Event::RunDone { run_id }).await;
        next = match after {
            After::Idle => idle(&inbox, &mut session, &mut lost_settings, &events).await,
            After::Next(command) => Some(command),
            After::Hangup => None,
        };
    }
    // Close, or every Runner dropped: the session is closed, never just dropped. No view
    // waits for an answer any more, so a write's failed commit can only be logged.
    if let Some(s) = session.take()
        && let Err(e) = s.close().await
    {
        log::warn!("closing the session: {e}");
    }
}

/// Waits for the next command, or None once every `Runner` is gone. An open session that
/// ends meanwhile (the server closed it, or keepalive found the network gone) is closed and
/// reported as `Disconnected`, so the tab does not show it as connected until the next Run.
async fn idle(
    inbox: &Receiver<Command>,
    session: &mut Option<Session>,
    lost: &mut bool,
    events: &Sender<Event>,
) -> Option<Command> {
    let Some(s) = session.as_ref() else {
        return inbox.recv().await.ok();
    };
    // Scoped so the wait's borrow of the session ends before it is taken.
    let command = {
        let (recv, closed) = (std::pin::pin!(inbox.recv()), std::pin::pin!(s.closed()));
        match select(recv, closed).await {
            Either::Left((command, _)) => Some(command.ok()),
            Either::Right(_) => None,
        }
    };
    if let Some(command) = command {
        return command;
    }
    if let Some(s) = drop_session(session, lost) {
        let _ = s.close().await;
    }
    let _ = events
        .send(Event::Disconnected {
            reason: LOST.to_owned(),
        })
        .await;
    inbox.recv().await.ok()
}

/// Opens the session if none is open, reporting `Connected` or `ConnectFailed`; an open
/// session is left alone and reports nothing.
async fn ensure_open(spec: &RunnerSpec, session: &mut Option<Session>, events: &Sender<Event>) {
    if session.is_some() {
        return;
    }
    match open(spec, |_| SessionOptions::default()).await {
        Ok(s) => {
            let _ = events
                .send(Event::Connected {
                    read_only: s.is_read_only(),
                    level: s.read_only_enforcement(),
                    engine: s.engine(),
                    cancel: s.cancel_handle(),
                })
                .await;
            *session = Some(s);
        }
        Err(message) => {
            let _ = events.send(Event::ConnectFailed { message }).await;
        }
    }
}

/// Why a session ends: a cut-short read-only result, or a lost connection.
const CUT_READ_ONLY: &str = "the read-only session closed to end a cut-short result";
const LOST: &str = "the session closed; any open transaction is lost";
/// Why the first run after a reopen that lost the session's settings was refused.
pub const SETTINGS_LOST: &str = "The connection was reopened and this session's USE/SET changes were lost. Run again to continue on the configured database.";

/// Ends a run's use of the session. A cut-short read-only result closes it outright; any
/// other run settles, reporting a drain error or closing the session past the drain cap.
async fn finish(
    session: &mut Option<Session>,
    lost: &mut bool,
    tail: Tail,
    run_id: u64,
    events: &Sender<Event>,
) {
    let Some(s) = session.as_mut() else { return };
    let reason = match tail {
        Tail::Lost => LOST.to_owned(),
        Tail::StoppedAtCap => ExecuteError::StoppedAtDrainCap.to_string(),
        Tail::Cut if s.is_read_only() => CUT_READ_ONLY.to_owned(),
        Tail::Clean | Tail::Cut => {
            if matches!(tail, Tail::Cut) {
                let _ = events.send(Event::Settling { run_id }).await;
            }
            match s.settle().await {
                Ok(()) => return,
                Err(ExecuteError::SessionClosed) => LOST.to_owned(),
                // A write stopped at the drain cap, or a session that ended: its own words say why.
                Err(e @ (ExecuteError::StoppedAtDrainCap | ExecuteError::SessionEnded { .. })) => {
                    e.to_string()
                }
                Err(e) => {
                    let message = e.to_string();
                    let _ = events.send(Event::SettleFailed { message }).await;
                    return;
                }
            }
        }
    };
    // The session is already lost or read-only here, so close has no write to report.
    if let Some(s) = drop_session(session, lost) {
        let _ = s.close().await;
    }
    let _ = events.send(Event::Disconnected { reason }).await;
}

/// Takes a session the runner will reopen later, marking `lost` if it carried USE/SET changes.
fn drop_session(session: &mut Option<Session>, lost: &mut bool) -> Option<Session> {
    let s = session.take();
    *lost |= s.as_ref().is_some_and(Session::settings_changed);
    s
}

/// Resolves the secret off the async threads (a keychain call can block) and connects. A
/// connect that failed without a secret names where to supply one, as `dbento run` does.
/// `options` picks the session's overrides from the resolved connection.
pub(crate) async fn open(
    spec: &RunnerSpec,
    options: impl FnOnce(&ConnectionConfig) -> SessionOptions + Send,
) -> Result<Session, String> {
    let Resolved {
        conn,
        secret,
        key_passphrase,
        hint,
    } = resolve(spec).await?;
    let mut options = options(&conn);
    options.key_passphrase = key_passphrase;
    connect(&spec.workspace, &conn, secret, options)
        .await
        .map_err(|e| match hint {
            Some(h) => format!("{e}\nhint: {h}"),
            None => e.to_string(),
        })
}

/// A connection with the secrets a session on it needs.
pub(crate) struct Resolved {
    pub(crate) conn: ConnectionConfig,
    pub(crate) secret: Option<Secret>,
    pub(crate) key_passphrase: Option<Secret>,
    /// Where a missing secret would come from; `None` when one was found.
    pub(crate) hint: Option<String>,
}

/// The spec's connection, its secret, its client key's passphrase and, when there is no
/// secret, a hint on where one would come from. The keychain lookups run on the blocking pool.
pub(crate) async fn resolve(spec: &RunnerSpec) -> Result<Resolved, String> {
    let missing = || format!("no connection `{}` in dbento.toml", spec.connection);
    let (ws, store, env, name) = (
        spec.workspace.clone(),
        spec.store.clone(),
        spec.env.clone(),
        spec.connection.clone(),
    );
    tokio::task::spawn_blocking(move || {
        ws.connection(&name).map(|conn| {
            let secret = resolve_secret(&ws, conn, &*store, &|v| env(v))?;
            let key_passphrase = resolve_key_passphrase(&ws, conn, &*store, &|v| env(v))?;
            let hint = match secret {
                None => secret_hint(&ws, conn, &*store),
                Some(_) => None,
            };
            Ok::<_, SecretError>(Resolved {
                conn: conn.clone(),
                secret,
                key_passphrase,
                hint,
            })
        })
    })
    .await
    .map_err(|_| "the secret lookup stopped".to_string())?
    .ok_or_else(missing)?
    .map_err(|e| e.to_string())
}

/// The registry key of the SQLite file `spec`'s connection opens; None for Postgres.
pub(crate) fn sqlite_file(spec: &RunnerSpec) -> Option<PathBuf> {
    let conn = spec.workspace.connection(&spec.connection)?;
    Some(parks::file_key(conn.sqlite_path()?))
}

/// What a `Run` or an `Explain` asks of the session.
enum Work {
    Script(String, AppLimits),
    Explain(ExplainRequest),
}

/// What ended a script: nothing, a command that must run next, or the view going away.
enum After {
    Idle,
    Next(Command),
    Hangup,
}

/// How the last statement a script executed left the session.
#[derive(Clone, Copy)]
enum Tail {
    Clean,
    /// It ended truncated or stopped, so rows may be left unread.
    Cut,
    /// The connection is gone.
    Lost,
    /// The connection was dropped to stop a write that outlasted the drain cap.
    StoppedAtCap,
}

impl Tail {
    /// The tail `end` leaves, or None when it leaves the previous one standing.
    fn of(end: &StatementEnd) -> Option<Tail> {
        match end {
            StatementEnd::Truncated(_) | StatementEnd::Stopped { .. } => Some(Tail::Cut),
            StatementEnd::Failed(ExecuteError::StoppedAtDrainCap) => Some(Tail::StoppedAtCap),
            StatementEnd::Failed(e) if e.closes_session() => Some(Tail::Lost),
            StatementEnd::Complete(_) | StatementEnd::Failed(_) | StatementEnd::Planned => {
                Some(Tail::Clean)
            }
            StatementEnd::Skipped => None,
        }
    }
}

/// One run's view of the runner's channels.
struct Ctx<'a> {
    run_id: u64,
    inbox: &'a Receiver<Command>,
    events: &'a Sender<Event>,
    tuning: RunnerTuning,
    /// The SQLite file the session reads, where its parked reads register; None on Postgres.
    file: Option<PathBuf>,
}

impl Ctx<'_> {
    /// Sends `event`; false once the view has gone away.
    async fn send(&self, event: Event) -> bool {
        self.events.send(event).await.is_ok()
    }

    /// Waits for a `Pull` or `Stop` of this run, a `Run`, an `Explain` or a `Close`, skipping anything
    /// else; None once the inbox closes.
    async fn wait_pull(&self) -> Option<Command> {
        self.wait_for(|c| matches!(c, Command::Pull { .. })).await
    }

    /// [`wait_pull`](Self::wait_pull) for a `Fetch` in place of a `Pull`.
    async fn wait_fetch(&self) -> Option<Command> {
        self.wait_for(|c| matches!(c, Command::Fetch { .. })).await
    }

    /// Waits for a command of this run that `wanted` accepts or a `Stop` of it, or for any
    /// `Run`, `Explain` or `Close`; every other command is dropped.
    async fn wait_for(&self, wanted: impl Fn(&Command) -> bool) -> Option<Command> {
        loop {
            let c = self.inbox.recv().await.ok()?;
            match &c {
                Command::Run { .. } | Command::Explain { .. } | Command::Close => return Some(c),
                Command::Stop { run_id } if *run_id == self.run_id => return Some(c),
                Command::Pull { run_id } | Command::Fetch { run_id, .. }
                    if *run_id == self.run_id && wanted(&c) =>
                {
                    return Some(c);
                }
                _ => {}
            }
        }
    }
}

/// Runs the statements of `sql` in order, as `dbento run` does: any error ends the script.
/// On a writable session an unguarded write waits for `Confirm` or `Skip`. Earlier
/// statements stream to their end; the last one parks and is pulled on demand. Also reports
/// how the last statement executed left the session.
async fn run_script(
    session: &mut Session,
    sql: &str,
    limits: AppLimits,
    cx: &Ctx<'_>,
) -> (After, Tail) {
    // A read-only session never asks: the engine refuses the write itself.
    let read_only = session.is_read_only();
    let ask = !read_only;
    let run_id = cx.run_id;
    let mut script = session.script(sql);
    let done = |stmt, end, elapsed| {
        cx.send(Event::StatementDone {
            run_id,
            stmt,
            end,
            elapsed,
        })
    };
    let mut core_limits = Limits::default();
    core_limits.max_rows = limits.max_rows;
    let mut stmt = 0;
    let mut tail = Tail::Clean;
    // Whether the statement just run was the last; a read-only last cut closes, not drains.
    let mut was_last = false;
    loop {
        // A new Run or Explain, a Close or this run's Stop sent mid-script abandons the
        // statements not yet begun.
        match cx.inbox.try_recv() {
            Ok(command @ (Command::Run { .. } | Command::Explain { .. } | Command::Close)) => {
                return (After::Next(command), tail);
            }
            Ok(Command::Stop { run_id: r }) if r == run_id => return (After::Idle, tail),
            Err(TryRecvError::Closed) => return (After::Hangup, tail),
            Ok(_) | Err(TryRecvError::Empty) => {} // a stray Confirm, Skip, Pull, Fetch or Stop
        }
        // Core drains a cut leftover in its next prepare, so the drain is announced first and,
        // once over, leaves only its error for `finish` to settle and report.
        if matches!(tail, Tail::Cut) && !(read_only && was_last) {
            cx.send(Event::Settling { run_id }).await;
            tail = Tail::Clean;
        }
        // The clock starts after preparing, as in `dbento run`, so a prepare error takes none.
        let mut pending = match script.next().await {
            None => return (After::Idle, tail),
            Some(Err(e)) => {
                let end = StatementEnd::Failed(e);
                tail = Tail::of(&end).unwrap_or(tail);
                done(stmt, end, Duration::ZERO).await;
                return (After::Idle, tail);
            }
            // Core settles any earlier leftover before it prepares, so nothing is left to drain.
            Some(Ok(p)) => {
                tail = Tail::Clean;
                p
            }
        };
        let text = pending.sql().to_owned();
        let kinds = pending.classification().unguarded.clone();
        if ask && !kinds.is_empty() {
            cx.send(Event::NeedsConfirm {
                run_id,
                stmt,
                sql: text.clone(),
                kinds,
                analyze: false,
            })
            .await;
            match answer(cx.inbox, run_id, stmt).await {
                Answer::Run => {}
                Answer::Skip => {
                    pending.skip();
                    done(stmt, StatementEnd::Skipped, Duration::ZERO).await;
                    stmt += 1;
                    continue;
                }
                Answer::Stop => {
                    pending.skip();
                    done(stmt, StatementEnd::Skipped, Duration::ZERO).await;
                    return (After::Idle, tail);
                }
                Answer::Interrupt(command) => return (After::Next(command), tail),
                Answer::Hangup => return (After::Hangup, tail),
            }
        }
        // Known before executing: only the last statement's stream may park.
        let last = pending.is_last().await;
        was_last = last;
        // A capped last read stays open at its cap for a Fetch. Otherwise only a read-only
        // session's last result may end by closing: any other leftover is drained, so a later
        // statement can run and a cut-short write still commits.
        let park =
            last && pending.classification().writes == Writes::No && limits.max_rows.is_some();
        core_limits.on_truncate = if park {
            OnTruncate::Park
        } else if read_only && last {
            OnTruncate::Close
        } else {
            OnTruncate::Drain
        };
        let started = Instant::now(); // the wait for an answer is not statement time
        cx.send(Event::Executing {
            run_id,
            stmt,
            sql: text.clone(),
        })
        .await;
        let mut stream = match pending.execute(core_limits).await {
            Ok(s) => s,
            Err(e) => {
                let end = StatementEnd::Failed(e);
                tail = Tail::of(&end).unwrap_or(tail);
                done(stmt, end, started.elapsed()).await;
                return (After::Idle, tail);
            }
        };
        cx.send(Event::StatementStarted {
            run_id,
            stmt,
            sql: text,
            columns: stream.columns().into(),
        })
        .await;
        let plan = Plan {
            stmt,
            last,
            park,
            budget: limits.max_result_bytes,
            started,
        };
        let mut tally = Tally::default();
        let end = match pump(&mut stream, cx, &plan, &mut tally).await {
            Flow::Ended(end) => end,
            // A cut stream still ends its statement, then the command that cut it runs. Once
            // parked at its cap, a user's cut ends it truncated with every row sent.
            Flow::Cut(reason, command) => {
                let end = match (reason, &tally.capped) {
                    (StopReason::User | StopReason::NewRun | StopReason::Closed, Some(o)) => {
                        let mut t = o.clone();
                        t.rows = tally.rows;
                        StatementEnd::Truncated(t)
                    }
                    _ => StatementEnd::Stopped {
                        rows: tally.rows,
                        reason,
                    },
                };
                done(stmt, end, plan.ran(&tally)).await;
                return (command.map_or(After::Idle, After::Next), Tail::Cut);
            }
            Flow::Hangup => return (After::Hangup, Tail::Cut),
        };
        // Once parked at its cap, a failed read keeps its rows: a timeout or cancel ends it
        // truncated and a lost connection stops it, so only another error fails it.
        let (end, after) = match (end, &tally.capped) {
            (StatementEnd::Failed(e), Some(o)) => {
                let mut t = o.clone();
                t.rows = tally.rows;
                match after_park(&e) {
                    kind @ (AfterPark::Expired | AfterPark::Idle) => {
                        let expired = Event::ParkExpired {
                            run_id,
                            statement_timeout: kind == AfterPark::Expired,
                        };
                        if !cx.send(expired).await {
                            return (After::Hangup, Tail::Cut);
                        }
                        (StatementEnd::Truncated(t), Some(Tail::Cut))
                    }
                    AfterPark::Truncated => (StatementEnd::Truncated(t), Some(Tail::Cut)),
                    AfterPark::Lost => (
                        StatementEnd::Stopped {
                            rows: tally.rows,
                            reason: StopReason::Lost,
                        },
                        Some(Tail::Lost),
                    ),
                    AfterPark::Failed => (StatementEnd::Failed(e), None),
                }
            }
            (end, _) => (end, None),
        };
        tail = after.or_else(|| Tail::of(&end)).unwrap_or(tail);
        let failed = matches!(end, StatementEnd::Failed(_));
        done(stmt, end, plan.ran(&tally)).await;
        if failed {
            return (After::Idle, tail);
        }
        stmt += 1;
    }
}

/// Explains `request` as statement 0 of its run. An ANALYZE that could write asks first on a
/// writable session, since it executes the statement; a read-only one lets the engine refuse.
async fn run_explain(session: &mut Session, request: &ExplainRequest, cx: &Ctx<'_>) -> After {
    let run_id = cx.run_id;
    let writes = (request.classification.as_ref())
        .filter(|c| request.mode == ExplainMode::Analyze && c.writes != Writes::No);
    if let Some(c) = writes
        && !session.is_read_only()
    {
        cx.send(Event::NeedsConfirm {
            run_id,
            stmt: 0,
            sql: request.statement.trim().to_owned(),
            kinds: c.unguarded.clone(),
            analyze: true,
        })
        .await;
        match answer(cx.inbox, run_id, 0).await {
            Answer::Run => {}
            Answer::Skip | Answer::Stop => {
                cx.send(Event::StatementDone {
                    run_id,
                    stmt: 0,
                    end: StatementEnd::Skipped,
                    elapsed: Duration::ZERO,
                })
                .await;
                return After::Idle;
            }
            Answer::Interrupt(command) => return After::Next(command),
            Answer::Hangup => return After::Hangup,
        }
    }
    let started = Instant::now();
    cx.send(Event::Executing {
        run_id,
        stmt: 0,
        sql: request.sql.clone(),
    })
    .await;
    // A Stop meanwhile cancels through the CancelHandle, so the explain itself ends early.
    let result = session.explain(request).await;
    cx.send(Event::Planned {
        run_id,
        result,
        elapsed: started.elapsed(),
    })
    .await;
    After::Idle
}

/// How a statement's stream ended inside the runner.
enum Flow {
    Ended(StatementEnd),
    /// Cut short; the `Run` or `Close` that cut it runs next, a `Stop` leaves None.
    Cut(StopReason, Option<Command>),
    Hangup,
}

/// How `command` cuts a stream of run `run_id`, if it does.
fn cut_by(command: Command, run_id: u64) -> Option<Flow> {
    match command {
        Command::Run { .. } | Command::Explain { .. } => {
            Some(Flow::Cut(StopReason::NewRun, Some(command)))
        }
        Command::Close => Some(Flow::Cut(StopReason::Closed, Some(command))),
        Command::Stop { run_id: r } if r == run_id => Some(Flow::Cut(StopReason::User, None)),
        _ => None,
    }
}

/// What `pump` knows of its statement before streaming it.
struct Plan {
    stmt: usize,
    last: bool,
    /// The stream runs with `OnTruncate::Park` and may wait at its row cap for a Fetch.
    park: bool,
    /// The batch heap the result may hold.
    budget: u64,
    started: Instant,
}

impl Plan {
    /// Active time so far: time parked waiting on the user is theirs, not the statement's.
    fn ran(&self, tally: &Tally) -> Duration {
        self.started.elapsed().saturating_sub(tally.parked)
    }
}

/// What `pump` has done so far.
#[derive(Default)]
struct Tally {
    rows: u64,
    parked: Duration,
    /// The outcome of the latest park at the row cap; None until the first.
    capped: Option<Outcome>,
}

/// Streams `stream` to the view. The last statement sends `first_rows` and one more batch,
/// then parks: each `Pull` releases one batch, and a park left idle for `idle_complete`
/// pulls the rest to its end or cap. Before parking it reads one batch ahead and holds it,
/// so a result that ends there finishes instead of parking. A batch that would take the sent
/// heap past the budget is dropped unsent and cuts the stream. At a parking row cap it waits
/// for a Fetch (see [`at_cap`]). A commit's release ends both waits: a paused stream reads
/// on to its end or cap, and a park at the cap ends there.
async fn pump(stream: &mut ResultStream<'_>, cx: &Ctx<'_>, plan: &Plan, tally: &mut Tally) -> Flow {
    let stmt = plan.stmt;
    // Registered for the stream's whole life, so a commit waits out a release in progress.
    let parked = (cx.file.clone()).filter(|_| plan.park).map(Parked::hold);
    let parked = parked.as_ref();
    // Batches the stream may still send before it parks; None until first_rows have gone out.
    let mut credit: Option<u32> = None;
    let mut parks = plan.last;
    let mut paused_sent = false;
    let mut held: Option<ResultBatch> = None;
    let mut heap: u64 = 0;
    loop {
        // Between batches a new Run, a Close or this run's Stop cuts the stream; any other
        // command is dropped.
        loop {
            match cx.inbox.try_recv() {
                // A Pull that beat the park is kept as its credit, so it is never lost.
                Ok(Command::Pull { run_id }) if run_id == cx.run_id && credit == Some(0) => {
                    credit = Some(1)
                }
                Ok(c) => {
                    if let Some(flow) = cut_by(c, cx.run_id) {
                        return flow;
                    }
                }
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Closed) => return Flow::Hangup,
            }
        }
        if parked.is_some_and(Parked::is_released) {
            parks = false;
        }
        if parks && credit == Some(0) {
            if held.is_none() {
                match stream.next_batch().await {
                    Ok(Some(batch)) => held = Some(batch),
                    Ok(None) => match at_cap(stream, cx, plan, tally, parked).await {
                        AtCap::Resume => {
                            parks = false;
                            continue;
                        }
                        AtCap::End(flow) => return flow,
                    },
                    Err(e) => return failed(e, cx),
                }
            }
            if !paused_sent {
                if !cx
                    .send(Event::Paused {
                        run_id: cx.run_id,
                        stmt,
                    })
                    .await
                {
                    return Flow::Hangup;
                }
                paused_sent = true;
            }
            // Each park waits afresh, so idle complete counts from the last batch sent.
            let since = Instant::now();
            let pulled = unless_released(
                parked,
                tokio::time::timeout(cx.tuning.idle_complete, cx.wait_pull()),
            )
            .await;
            tally.parked += since.elapsed();
            match pulled {
                None | Some(Err(_)) => parks = false,
                Some(Ok(Some(Command::Pull { .. }))) => credit = Some(1),
                Some(Ok(Some(c))) => match cut_by(c, cx.run_id) {
                    Some(flow) => return flow,
                    None => continue, // park again
                },
                Some(Ok(None)) => return Flow::Hangup,
            }
        }
        // A held batch goes out first; a cut drops it unsent.
        let next = match held.take() {
            Some(batch) => Ok(Some(batch)),
            None => stream.next_batch().await,
        };
        match next {
            Ok(Some(batch)) => {
                // Only a sent batch counts, so a held one is weighed as it goes out.
                heap = heap.saturating_add(batch.heap_bytes() as u64);
                if heap > plan.budget {
                    return Flow::Cut(StopReason::MemoryBudget, None);
                }
                tally.rows += batch.len() as u64;
                if !cx
                    .send(Event::Batch {
                        run_id: cx.run_id,
                        stmt,
                        batch,
                    })
                    .await
                {
                    return Flow::Hangup;
                }
                credit = match credit {
                    None if tally.rows >= cx.tuning.first_rows => Some(1),
                    None => None,
                    Some(c) => Some(c.saturating_sub(1)),
                };
            }
            Ok(None) => match at_cap(stream, cx, plan, tally, parked).await {
                // A fetch streams eagerly, with no first-rows park.
                AtCap::Resume => parks = false,
                AtCap::End(flow) => return flow,
            },
            Err(e) => return failed(e, cx),
        }
    }
}

/// Whether a stream that stopped yielding batches resumes or ends.
enum AtCap {
    Resume,
    End(Flow),
}

/// Handles a stream that returned its last batch. A parking statement cut at its row cap
/// reports `ParkedAtCap` and waits up to `park_at_cap` for a Fetch, which extends it; an
/// expiry or a commit's release ends it truncated, and a Stop, Run or Close cuts it. Any
/// other stream just ends.
async fn at_cap(
    stream: &mut ResultStream<'_>,
    cx: &Ctx<'_>,
    plan: &Plan,
    tally: &mut Tally,
    parked: Option<&Parked>,
) -> AtCap {
    let capped = stream
        .outcome()
        .filter(|o| plan.park && o.truncated_by == Some(TruncatedBy::Rows))
        .cloned();
    let Some(outcome) = capped else {
        return AtCap::End(Flow::Ended(end_of(stream)));
    };
    tally.capped = Some(outcome.clone());
    let reached = Event::ParkedAtCap {
        run_id: cx.run_id,
        stmt: plan.stmt,
        outcome: outcome.clone(),
        elapsed: plan.ran(tally),
    };
    if !cx.send(reached).await {
        return AtCap::End(Flow::Hangup);
    }
    // One timer per park: a Fetch that re-parks starts a fresh one.
    let since = Instant::now();
    let fetched = unless_released(
        parked,
        tokio::time::timeout(cx.tuning.park_at_cap, cx.wait_fetch()),
    )
    .await;
    tally.parked += since.elapsed();
    let command = match fetched {
        None => {
            let released = Event::ParkReleased { run_id: cx.run_id };
            if !cx.send(released).await {
                return AtCap::End(Flow::Hangup);
            }
            return AtCap::End(Flow::Ended(StatementEnd::Truncated(outcome)));
        }
        Some(Err(_)) => {
            let expired = Event::ParkExpired {
                run_id: cx.run_id,
                statement_timeout: false,
            };
            if !cx.send(expired).await {
                return AtCap::End(Flow::Hangup);
            }
            return AtCap::End(Flow::Ended(StatementEnd::Truncated(outcome)));
        }
        Some(Ok(None)) => return AtCap::End(Flow::Hangup),
        Some(Ok(Some(c))) => c,
    };
    match command {
        Command::Fetch { more, .. } => match stream.extend(more).await {
            Ok(()) => AtCap::Resume,
            Err(_) => AtCap::End(Flow::Ended(StatementEnd::Failed(
                ExecuteError::SessionClosed,
            ))),
        },
        // wait_fetch returns only this run's Stop, a Run or a Close besides a Fetch.
        c => AtCap::End(cut_by(c, cx.run_id).unwrap_or(Flow::Hangup)),
    }
}

/// Awaits `wait`, or None once `parked` is released first (at once if it already is). The
/// waits it wraps only receive from the inbox, which loses no command when dropped.
async fn unless_released<T>(parked: Option<&Parked>, wait: impl Future<Output = T>) -> Option<T> {
    let Some(parked) = parked else {
        return Some(wait.await);
    };
    match select(std::pin::pin!(parked.released()), std::pin::pin!(wait)).await {
        Either::Left(_) => None,
        Either::Right((value, _)) => Some(value),
    }
}

/// How a failed read ends the stream. A Stop sent with a cancel makes the read fail, so a
/// cancelled read yields to a cut waiting in the inbox.
fn failed(e: ExecuteError, cx: &Ctx<'_>) -> Flow {
    if matches!(e, ExecuteError::Cancelled) {
        while let Ok(command) = cx.inbox.try_recv() {
            if let Some(flow) = cut_by(command, cx.run_id) {
                return flow;
            }
        }
    }
    Flow::Ended(StatementEnd::Failed(e))
}

/// How a read that fails after its first park at the cap ends.
#[derive(Debug, PartialEq, Eq)]
enum AfterPark {
    Expired,
    /// The server dropped the parked result after it sat unread too long.
    Idle,
    Truncated,
    Lost,
    Failed,
}

fn after_park(e: &ExecuteError) -> AfterPark {
    match e {
        ExecuteError::Timeout => AfterPark::Expired,
        ExecuteError::ResultExpired => AfterPark::Idle,
        ExecuteError::Cancelled => AfterPark::Truncated,
        ExecuteError::SessionClosed
        | ExecuteError::SessionEnded { .. }
        | ExecuteError::StoppedAtDrainCap => AfterPark::Lost,
        ExecuteError::Database { code, sqlstate, .. }
            if lost_connection(code, sqlstate.as_deref()) =>
        {
            AfterPark::Lost
        }
        _ => AfterPark::Failed,
    }
}

/// Whether a database error means the connection is gone: Postgres class 08 and 57P01-57P03
/// by `code`; on MySQL (a SQLSTATE present) class 08 or error number 4031, 1927 or 3169.
fn lost_connection(code: &str, sqlstate: Option<&str>) -> bool {
    match sqlstate {
        // MySQL: a connection-exception SQLSTATE, or the interaction-timeout and killed-session
        // numbers whose SQLSTATE is generic. The numbers count only beside a SQLSTATE, so an
        // SQLite extended code of the same digits is never misread.
        Some(state) => state.starts_with("08") || matches!(code, "4031" | "1927" | "3169"),
        // Postgres: connection exceptions and the admin, crash and cannot-connect shutdowns.
        None => code.starts_with("08") || matches!(code, "57P01" | "57P02" | "57P03"),
    }
}

/// How a finished stream ended, from its outcome.
fn end_of(stream: &ResultStream<'_>) -> StatementEnd {
    match stream.outcome().cloned() {
        Some(o) if o.truncated => StatementEnd::Truncated(o),
        Some(o) => StatementEnd::Complete(o),
        // A finished stream always has an outcome; a driver bug reads as a lost session.
        None => StatementEnd::Failed(ExecuteError::SessionClosed),
    }
}

enum Answer {
    Run,
    Skip,
    Stop,
    Interrupt(Command),
    Hangup,
}

/// Waits for the answer to statement `stmt` of run `run_id`; this run's Stop ends the script,
/// a new Run or a Close abandons it, and answers meant for anything else are ignored.
async fn answer(inbox: &Receiver<Command>, run_id: u64, stmt: usize) -> Answer {
    loop {
        match inbox.recv().await {
            Err(_) => return Answer::Hangup,
            Ok(Command::Confirm { run_id: r, stmt: s }) if (r, s) == (run_id, stmt) => {
                return Answer::Run;
            }
            Ok(Command::Skip { run_id: r, stmt: s }) if (r, s) == (run_id, stmt) => {
                return Answer::Skip;
            }
            Ok(Command::Stop { run_id: r }) if r == run_id => return Answer::Stop,
            Ok(command @ (Command::Run { .. } | Command::Explain { .. } | Command::Close)) => {
                return Answer::Interrupt(command);
            }
            Ok(_) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{AfterPark, after_park, lost_connection};
    use dbento_core::ExecuteError;

    #[test]
    fn lost_connections_read_from_postgres_codes_and_mysql_sqlstates_and_numbers() {
        for (code, sqlstate) in [
            ("08006", None),
            ("08003", None),
            ("57P01", None),
            ("57P02", None),
            ("57P03", None),
            ("1053", Some("08S01")),
            ("1080", Some("08S01")),
            ("4031", Some("HY000")),
            ("1927", Some("70100")),
            ("3169", Some("HY000")),
        ] {
            assert!(lost_connection(code, sqlstate), "{code} {sqlstate:?}");
        }
        for (code, sqlstate) in [
            ("42P01", None),
            ("1146", Some("42S02")),
            ("1064", Some("42000")),
            ("1317", Some("70100")),
            ("4031", None),
            ("1927", None),
            ("3169", None),
            ("1", None),
        ] {
            assert!(!lost_connection(code, sqlstate), "{code} {sqlstate:?}");
        }
    }

    #[test]
    fn a_failed_read_after_the_park_ends_by_its_kind() {
        assert_eq!(after_park(&ExecuteError::Timeout), AfterPark::Expired);
        assert_eq!(after_park(&ExecuteError::Cancelled), AfterPark::Truncated);
        assert_eq!(after_park(&ExecuteError::SessionClosed), AfterPark::Lost);
        assert_eq!(
            after_park(&ExecuteError::StoppedAtDrainCap),
            AfterPark::Lost
        );
        assert_eq!(after_park(&ExecuteError::ResultExpired), AfterPark::Idle);
        assert_eq!(
            after_park(&ExecuteError::SessionEnded {
                reason: "lost".to_owned()
            }),
            AfterPark::Lost
        );
        assert_eq!(
            after_park(&ExecuteError::database("57P01", "terminating connection")),
            AfterPark::Lost
        );
        assert_eq!(
            after_park(&ExecuteError::database("1146", "no such table")),
            AfterPark::Failed
        );
    }
}
