//! A query tab's state: its runner, the current run's statements and any pending question.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use async_channel::Receiver;
use dbento_core::{
    Applied, ApplyError, Browse, CancelHandle, CellRef, Change, ChangeOutcome, ColumnInfo,
    EditBatch, EditTarget, EditValue, Engine, ExecuteError, ExplainMode, ExplainOutput,
    ExplainRequest, ForeignKey, HistoryEntry, HistoryOutcome, Keyset, Outcome, ReadOnlyEnforcement,
    RelationRef, RowKey, Sort, Unguarded, Workspace, browse_sql, render_sql, result_ranges,
    statement_count,
};
use gpui_kit::{Context, Entity, EventEmitter, Pixels, Task};

use crate::browse::{self, BrowseEvent, BrowseState, EditBlock};
use crate::catalog::catalog_options;
use crate::grid::{ColumnView, fit_width_at};
use crate::history::{HistoryLog, history_outcome};
use crate::key_view::KeyView;
use crate::link::LinkState;
use crate::parks;
use crate::results::ResultStore;
use crate::runner::{
    self, AppLimits, Command, Event, KeyValueAnswer, Runner, RunnerSpec, RunnerTuning,
    SourceColumns, StatementEnd, StopReason,
};
use crate::settings;
use crate::staged::{self, EditHistory, GridRow, StagedEdits};
use crate::tokio_bridge;

/// The row limits the `row-limit` button cycles through; None leaves only the memory budget.
pub const ROW_LIMITS: [Option<u64>; 5] = [
    Some(1_000),
    Some(10_000),
    Some(100_000),
    Some(1_000_000),
    None,
];

/// The first `ROW_LIMITS` step above `cap`; None above the largest step or with no cap.
pub fn raise_target(cap: Option<u64>) -> Option<u64> {
    ROW_LIMITS
        .iter()
        .flatten()
        .copied()
        .find(|&l| cap.is_some_and(|c| l > c))
}

/// At most `max_rows` rows of `sql`'s one statement, read on `session`.
async fn read_rows(
    session: &mut dbento_core::Session,
    sql: &str,
    max_rows: u64,
) -> Result<ResultStore, String> {
    let mut script = session.script(sql);
    let pending = match script.next().await {
        Some(pending) => pending.map_err(|e| e.to_string())?,
        None => return Err("no statement to run".into()),
    };
    let mut limits = dbento_core::Limits::default();
    limits.max_rows = Some(max_rows);
    let mut stream = pending.execute(limits).await.map_err(|e| e.to_string())?;
    let mut store = ResultStore::new(stream.columns().into());
    while let Some(batch) = stream.next_batch().await.map_err(|e| e.to_string())? {
        store.push(batch);
    }
    Ok(store)
}

/// How long a COMMIT waits for reads parked on its SQLite file to let go; the busy timeout
/// then still covers a reset in flight.
const RELEASE_WAIT: Duration = Duration::from_secs(5);

/// How a failed reload's notice begins, so a later successful one can clear it.
const RELOAD_FAILED: &str = "Could not reload";

/// The notice of a browse loaded again because its table's columns changed under it.
pub const COLUMNS_RELOADED: &str =
    "The table's columns changed since it loaded, so it was loaded again.";

/// The notice of a column change found with edits staged; saving stays off until a fresh load.
pub const COLUMNS_BLOCKED: &str = "The table's columns changed since it loaded. Saving is off \
    so no staged edit lands on the wrong column; run the table again to load it fresh.";

/// One statement of the current run.
pub struct StatementResult {
    /// The statement's index in the script.
    pub stmt: usize,
    /// Its text; for a statement that failed to prepare, its slice of the run's text.
    pub sql: String,
    /// The engine it ran on, which a later connection switch does not change.
    pub engine: Option<Engine>,
    /// The tab's [`TabModel::epoch`] it ran in; another epoch's session may be another database.
    pub epoch: u64,
    /// Its columns, rows and end.
    pub store: ResultStore,
    /// Execution time, once it has ended.
    pub elapsed: Option<Duration>,
    /// Where `sql` sits in the editor when the run came from it ([`statement_source`]).
    pub source: Option<Range<usize>>,
    /// Set when the statement was explained rather than run; such a result is never recorded.
    pub plan: Option<PlanResult>,
}

/// An explain's result: the mode it ran in and, once it ended well, the database's output.
#[derive(Debug, Clone)]
pub struct PlanResult {
    /// The mode sent to the database.
    pub mode: ExplainMode,
    /// `Analyze` was asked on SQLite, which only plans.
    pub analyze_unavailable: bool,
    /// The output, set by a successful explain.
    pub output: Option<Arc<ExplainOutput>>,
}

impl PlanResult {
    /// A plan result with no output yet.
    pub fn new(mode: ExplainMode, analyze_unavailable: bool) -> PlanResult {
        PlanResult {
            mode,
            analyze_unavailable,
            output: None,
        }
    }
}

/// The editor range of result `stmt` of run text `text` that began at editor offset
/// `start`: its `result_ranges` range trimmed at both ends (so it spans exactly `sql`);
/// None past the last result.
pub fn statement_source(
    engine: Engine,
    text: &str,
    start: usize,
    stmt: usize,
) -> Option<Range<usize>> {
    let range = result_ranges(engine, text).get(stmt)?.clone();
    let piece = &text[range.clone()];
    let lead = piece.len() - piece.trim_start().len();
    let len = piece.trim().len();
    let from = start + range.start + lead;
    Some(from..from + len)
}

/// An unguarded write waiting for the user to run or skip it.
pub struct PendingConfirm {
    /// The run it belongs to.
    pub run_id: u64,
    /// The statement's index in the script.
    pub stmt: usize,
    /// Its text.
    pub sql: String,
    /// Why it needs a confirmation.
    pub kinds: Vec<Unguarded>,
    /// The question is for an explain's ANALYZE, which runs the statement and rolls it back.
    pub analyze: bool,
}

/// What a discard question holds back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Discard {
    /// Running this SQL.
    Run(String),
    /// Explaining this request.
    Explain(ExplainRequest),
    /// Applying this browse filter.
    Filter(String),
    /// Cycling the sort of this column.
    Sort(String),
    /// Setting the browse sort to this one outright.
    SetSort(Option<Sort>),
    /// Leaving the browse state because the editor's SQL was edited.
    Edit,
}

/// A "Discard N pending changes?" question waiting on Keep or Discard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingDiscard {
    /// Distinguishes successive questions, so a view can tell a new one from the one it showed.
    pub id: u64,
    /// The question, `staged::discard_title` of the staged count.
    pub title: String,
    /// What Discard goes on to do.
    pub action: Discard,
}

/// Resolves once the runner has closed its session and ended.
pub struct Closing(Option<Receiver<Event>>);

impl Closing {
    /// Reads and discards the runner's events until its channel closes: the channel is
    /// bounded, so a runner blocked on a full one could otherwise never reach its close.
    pub async fn wait(self) {
        if let Some(events) = self.0 {
            while events.recv().await.is_ok() {}
        }
    }
}

/// Why `switch_connection` left the tab where it was.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SwitchBlocked {
    /// The tab is closed.
    Closed,
    /// The tab already runs on that connection.
    Same,
    /// The workspace has no connection by that name.
    Unknown,
    /// The tab browses a table, whose staged edits belong to its connection.
    Browse,
}

/// Why a stream parked at its row cap was closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Expiry {
    /// No Fetch arrived within the park's wait.
    Idle,
    /// The statement timeout fired once reading resumed.
    StatementTimeout,
    /// A commit to the same SQLite file asked the park to let go of its lock.
    Released,
}

/// Where a statement stands once it has parked at its row cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapView {
    /// Parked; `rows` is the count at this park.
    Parked {
        /// Rows delivered so far.
        rows: u64,
    },
    /// A Fetch is pulling past the park it left.
    Fetching,
    /// Ended after parking at least once; `expired` says whether a park expiry closed it.
    Ended {
        /// Set when a park expiry, not the user or the stream's end, closed it.
        expired: Option<Expiry>,
    },
}

/// How an expired browse result continues past its loaded rows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Continuation {
    /// Unsorted: the browse re-runs after the last loaded row's key.
    Keyset,
    /// Sorted: the same browse SQL re-runs and its first `n` rows, those already loaded, are dropped.
    Skip(u64),
}

/// A continuation in flight: its rows append to result 0 instead of replacing it.
struct Append {
    /// Rows a sorted re-run repeats from its start, dropped as they arrive.
    skip: u64,
    dropped: u64,
    /// Result 0's latest cap outcome and the expiry that closed it, restored when the
    /// continuation fails or is stopped so it can be tried again.
    outcome: Outcome,
    expired: Expiry,
    /// Its columns differ from result 0's, so its rows are dropped and the run is stopped.
    drifted: bool,
    /// Result 0's time before it, which the continuation's own time adds to.
    base: Duration,
}

/// The current run's statement that has parked at its row cap at least once.
struct Cap {
    stmt: usize,
    /// The outcome reported at the latest park, the base of a `Truncated` end.
    outcome: Outcome,
    fetching: bool,
    ended: bool,
    expired: Option<Expiry>,
}

/// The model behind one query tab. It owns the runner and the pump that applies the
/// runner's events, and drops events from any run but the latest.
pub struct TabModel {
    spec: RunnerSpec,
    runner: Option<Runner>,
    pump: Option<Task<()>>,
    cancel: Option<CancelHandle>,
    run_id: u64,
    /// Bumped whenever the spec's connection or workspace is replaced, so a result can tell
    /// whether the session now open still reaches the database it ran on.
    epoch: u64,
    running: bool,
    read_only: Option<bool>,
    /// The session's read-only level, set and cleared with `read_only`.
    read_only_level: Option<ReadOnlyEnforcement>,
    /// A session is open: set by `Connected`, cleared once it ends. `read_only` outlives it,
    /// since the edit rules keep the last known mode.
    connected: bool,
    /// The open session reported `Event::SettingsChanged`; a new session clears it.
    settings_changed: bool,
    /// The tab dropped a session whose settings had changed; the next run is refused once.
    lost_settings: bool,
    /// The open session's database label while a USE keeps it off the configured one.
    current_database: Option<String>,
    results: Vec<StatementResult>,
    confirm: Option<PendingConfirm>,
    error: Option<String>,
    /// The parked statement's script index; each `pull` releases one more batch of it.
    paused: Option<usize>,
    /// A `Pull` is in flight; the next batch or end clears it, so scrolling sends one at a time.
    pulling: bool,
    /// The statement that has parked at its row cap in this run, and where it stands.
    cap: Option<Cap>,
    /// A read-write Stop of the parked result is draining; a further Cancel fires the handle.
    stopping: bool,
    settling: bool,
    /// The run was stopped at a confirmation and executes nothing more, so Cancel has no target.
    withdrawn: bool,
    cancels_fired: u64,
    notice: Option<String>,
    max_rows: Option<u64>,
    max_result_bytes: u64,
    limits: AppLimits,
    tuning: RunnerTuning,
    /// The current run's engine and text, to name a statement that fails to prepare.
    run_text: Option<(Engine, String)>,
    /// The editor offset the current run's text began at, for each statement's `source`.
    run_start: usize,
    /// The current run is an explain in this mode, with `analyze_unavailable`.
    explain: Option<(ExplainMode, bool)>,
    /// The run start a pending `Discard::Run` or `Discard::Explain` question was asked with.
    discard_start: usize,
    /// Closed for good: a run would open a session nothing waits to close.
    closed: bool,
    history: Option<Entity<HistoryLog>>,
    /// When each statement of the current run first reported, for its history entry.
    started: HashMap<usize, SystemTime>,
    /// Entries of ended statements not yet handed to the history log.
    unrecorded: Vec<HistoryEntry>,
    /// The table this tab browses, with its filter and sort.
    browse: Option<BrowseState>,
    /// The relation asked of the runner whose description has not landed yet.
    target_pending: Option<RelationRef>,
    /// The filter the pending target's first browse carries.
    target_filter: Option<String>,
    /// The sort the pending target's first browse carries.
    target_sort: Option<Sort>,
    /// The pending target waits for the run it stopped to end before it is asked for.
    target_queued: bool,
    /// A restored table whose target the next run loads, with the browse it resumes.
    deferred_target: Option<(RelationRef, Browse)>,
    /// A landed description (or why there is none), waiting for `flush_browse` to act on it.
    target_landed: Option<Result<(EditTarget, Vec<ForeignKey>), String>>,
    filter_error: Option<String>,
    /// Staged edits of the browse result, keyed by result row.
    edits: StagedEdits,
    /// Undo and redo steps of `edits`.
    edit_history: EditHistory,
    save: SaveState,
    /// The apply or the commit/rollback in flight; dropping it aborts the work.
    save_task: Option<Task<()>>,
    /// The open transaction waiting on the prompt's answer.
    applied: Option<Applied>,
    prompt: Option<ApplyPrompt>,
    prompts: u64,
    banner: Option<String>,
    /// The statement a Fetch or continuation is loading more of, and when it began.
    ticking: Option<(usize, Instant)>,
    /// The result row of each change of the batch in flight, in batch order.
    save_rows: Vec<GridRow>,
    /// The run id when the prompt was answered; a newer run skips the commit's re-run.
    finish_run: u64,
    discard: Option<PendingDiscard>,
    discards: u64,
    /// The continuation the current run is, if any; a new run or a retired runner clears it.
    append: Option<Append>,
    /// Committed updates waiting for the commit's re-run to start.
    kept_next: Vec<KeptRow>,
    /// The current run's candidates, checked against its first statement's complete rows.
    kept_check: Vec<KeptRow>,
    /// Committed rows the commit's complete re-run no longer loads; `kept_rows`.
    kept: Vec<KeptRow>,
    /// Loaded rows whose change came back a conflict in the last rejected save.
    conflicts: Vec<GridRow>,
    /// The conflicting rows' reload in flight; dropping it abandons the read.
    reload_task: Option<Task<()>>,
    /// What the last reload of the conflicting rows found.
    fresh: Option<FreshRows>,
    /// Hidden columns and set widths per result index; one applies while its names match.
    column_views: HashMap<usize, ColumnView>,
    /// The browsed table's columns no longer match result 0's; saving and continuing stay
    /// off until a fresh load clears it.
    stale: bool,
    /// The next run is the fresh load a column change asked for, so it says so.
    announce_fresh: bool,
    /// The Redis key this tab shows the value of, read page by page into result 0.
    key_view: Option<KeyView>,
    /// The value page in flight; dropping it drops its answer.
    key_task: Option<Task<()>>,
}

/// The conflicting rows as a reload read them, and which fresh row each loaded row is now.
struct FreshRows {
    store: ResultStore,
    /// Loaded row to its fresh row; None when no row has its key any more.
    rows: HashMap<u64, Option<u64>>,
}

/// A committed updated row the post-commit re-run no longer loads, and the band's text for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeptRow {
    /// The row's identity after the commit, with any edited key column at its new value.
    pub key: RowKey,
    /// `staged::kept_label` of the loaded key and the committed cells.
    pub label: String,
}

/// Where a save of staged edits stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveState {
    /// No save in flight.
    Idle,
    /// A parked stream is being stopped before the apply starts.
    Stopping,
    /// The batch runs on its own session.
    Applying,
    /// The transaction is open and the prompt waits for Commit or Roll back.
    Prompt,
    /// The COMMIT or ROLLBACK runs.
    Finishing,
}

/// Which statement Copy as renders for a row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CopyKind {
    /// An INSERT of the row's shown values.
    Insert,
    /// An UPDATE setting the shown values, keyed by the loaded identity.
    Update,
    /// A DELETE keyed by the loaded identity.
    Delete,
}

/// The question an applied batch asks: its title, the affected-rows line and the SQL.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApplyPrompt {
    /// Distinguishes successive prompts, so a view can tell a new one from the one it showed.
    pub id: u64,
    /// "Apply changes to schema.table".
    pub title: String,
    /// How many rows the batch affected, and that locks are held.
    pub detail: String,
    /// The batch's statements, capped for the preview.
    pub sql: String,
}

/// An apply's answer: core's (`Err(Ok)`) or a secret or workspace failure (`Err(Err)`).
type ApplyResult = Result<Result<Applied, Result<ApplyError, String>>, tokio_bridge::JoinError>;

impl EventEmitter<BrowseEvent> for TabModel {}

impl TabModel {
    /// A tab over `spec`; no runner starts until the first connect or run.
    pub fn new(spec: RunnerSpec) -> Self {
        Self::with_tuning(spec, RunnerTuning::default())
    }

    /// A tab whose runners use `tuning`; `new` uses the default.
    pub fn with_tuning(spec: RunnerSpec, tuning: RunnerTuning) -> Self {
        TabModel {
            spec,
            runner: None,
            pump: None,
            cancel: None,
            run_id: 0,
            epoch: 0,
            running: false,
            read_only: None,
            read_only_level: None,
            connected: false,
            settings_changed: false,
            lost_settings: false,
            current_database: None,
            results: Vec::new(),
            confirm: None,
            error: None,
            paused: None,
            pulling: false,
            cap: None,
            stopping: false,
            settling: false,
            withdrawn: false,
            cancels_fired: 0,
            notice: None,
            max_rows: AppLimits::default().max_rows,
            max_result_bytes: AppLimits::default().max_result_bytes,
            limits: AppLimits::default(),
            tuning,
            run_text: None,
            run_start: 0,
            explain: None,
            discard_start: 0,
            closed: false,
            history: None,
            started: HashMap::new(),
            unrecorded: Vec::new(),
            browse: None,
            target_pending: None,
            target_filter: None,
            target_sort: None,
            target_queued: false,
            deferred_target: None,
            target_landed: None,
            filter_error: None,
            edits: StagedEdits::default(),
            edit_history: EditHistory::default(),
            save: SaveState::Idle,
            save_task: None,
            applied: None,
            prompt: None,
            prompts: 0,
            banner: None,
            ticking: None,
            save_rows: Vec::new(),
            finish_run: 0,
            discard: None,
            discards: 0,
            append: None,
            kept_next: Vec::new(),
            kept_check: Vec::new(),
            kept: Vec::new(),
            conflicts: Vec::new(),
            reload_task: None,
            fresh: None,
            column_views: HashMap::new(),
            stale: false,
            announce_fresh: false,
            key_view: None,
            key_task: None,
        }
    }

    /// Opens the session ahead of the first run. A failure shows as `error()`, and the next
    /// run retries. Does nothing once closed.
    pub fn connect(&mut self, cx: &mut Context<Self>) {
        if self.closed {
            return;
        }
        if self.runner.is_none() {
            self.start_runner(cx);
        }
        if let Some(r) = &self.runner {
            r.send(Command::Connect);
        }
    }

    /// Runs every statement of `sql` as a new run, cancelling one still in progress and
    /// clearing the previous results. A stopped runner is replaced first. Refused while a
    /// commit settles with edits staged.
    pub fn run(&mut self, sql: String, cx: &mut Context<Self>) {
        self.run_at(sql, 0, cx);
    }

    /// `run`, for text that begins at editor offset `start`, so each result knows its range.
    pub fn run_at(&mut self, sql: String, start: usize, cx: &mut Context<Self>) {
        if !self.begin_run(&sql, start, cx) {
            return;
        }
        if let Some(r) = &self.runner {
            r.send(Command::Run {
                run_id: self.run_id,
                sql,
                limits: self.limits,
            });
        }
        cx.notify();
    }

    /// Explains `request` as a new run, as `run_at` runs text; `start` is the editor offset
    /// of `request.statement`. True once sent; false when it asks to discard staged edits.
    pub fn request_explain(
        &mut self,
        request: ExplainRequest,
        start: usize,
        cx: &mut Context<Self>,
    ) -> bool {
        let asked = self.discard.is_some();
        if !self.ask_discard(Discard::Explain(request.clone()), cx) {
            if !asked {
                self.discard_start = start;
            }
            return false;
        }
        self.explain_at(request, start, cx);
        true
    }

    /// Sends `request` as a new run; the Explain counterpart of `run_at`.
    fn explain_at(&mut self, request: ExplainRequest, start: usize, cx: &mut Context<Self>) {
        if !self.begin_run(&request.statement, start, cx) {
            return;
        }
        self.explain = Some((request.mode, request.analyze_unavailable));
        if let Some(r) = &self.runner {
            r.send(Command::Explain {
                run_id: self.run_id,
                request,
            });
        }
        cx.notify();
    }

    /// Resets the tab for a new run of `text` at editor offset `start`, cancelling the one
    /// in progress. False when the run may not start (closed, a commit holding edits, or a
    /// stale browse that reloads its table instead).
    fn begin_run(&mut self, text: &str, start: usize, cx: &mut Context<Self>) -> bool {
        if self.closed || self.commit_holds_edits() {
            return false;
        }
        // The session this tab dropped had switched database or settings; a run now would land
        // silently on the configured ones, so the first one is answered with why instead.
        if std::mem::take(&mut self.lost_settings) {
            self.notice = Some(runner::SETTINGS_LOST.into());
            cx.notify();
            return false;
        }
        // Re-running a browse whose table changed shape describes the table afresh first, so
        // the run that follows edits against the table's current columns.
        if self.stale && self.browse.is_some() {
            self.edits.clear();
            self.edit_history.clear();
            self.banner = None;
            self.clear_conflicts();
            self.fresh_load(cx);
            return false;
        }
        self.abort_save(cx);
        // The new run replaces the one a queued target was waiting on, and the target with it.
        if std::mem::take(&mut self.target_queued) {
            self.target_pending = None;
            self.target_filter = None;
            self.target_sort = None;
        }
        // A statement waiting on a question is not executing, and the runner abandons it on
        // the new Run; firing the handle then would leave a Postgres cancel armed.
        // A parked stream is not executing either: the new Run itself cuts it. A run stopped
        // at a question executes nothing, so its cancel would hit the new run's statement.
        if self.running() && self.confirm.is_none() && !self.withdrawn {
            self.fire_cancel();
        }
        if self.runner.is_none() {
            self.start_runner(cx);
        }
        // The replaced run's own ends are dropped by run id, so its unfinished statements
        // are recorded here.
        self.record_unfinished(StopReason::NewRun);
        self.flush_history(cx);
        self.started.clear();
        self.run_id += 1;
        self.running = true;
        self.append = None;
        self.ticking = None;
        self.results.clear();
        // Only the commit's own re-run checks its candidates; any other run drops them.
        self.kept.clear();
        self.kept_check = std::mem::take(&mut self.kept_next);
        self.confirm = None;
        self.paused = None;
        self.pulling = false;
        self.cap = None;
        self.stopping = false;
        self.withdrawn = false;
        self.notice = std::mem::take(&mut self.announce_fresh).then(|| COLUMNS_RELOADED.into());
        self.stale = false;
        self.edits.clear();
        self.edit_history.clear();
        self.banner = None;
        self.clear_conflicts();
        self.discard = None;
        self.limits = AppLimits {
            max_rows: self.max_rows,
            max_result_bytes: self.max_result_bytes,
        };
        self.run_text = self
            .spec
            .workspace
            .connection(&self.spec.connection)
            .map(|c| (c.engine(), text.to_owned()));
        self.run_start = start;
        self.explain = None;
        true
    }

    /// A COMMIT or ROLLBACK is settling with edits staged. A failed COMMIT keeps them under
    /// its banner, so a run, which drops the edits, is refused until the outcome lands.
    fn commit_holds_edits(&self) -> bool {
        self.save == SaveState::Finishing && !self.edits.is_empty()
    }

    /// Runs (`run`) the statement waiting on a confirmation, or cancels: the statement is
    /// skipped and the whole run stops, as Cancel does everywhere else.
    pub fn answer(&mut self, run: bool, cx: &mut Context<Self>) {
        let (Some(p), Some(r)) = (self.confirm.take(), &self.runner) else {
            return;
        };
        let (run_id, stmt) = (p.run_id, p.stmt);
        if run {
            r.send(Command::Confirm { run_id, stmt });
        } else {
            r.send(Command::Stop { run_id });
            self.withdrawn = true;
        }
        cx.notify();
    }

    /// Asks the runner for one more batch of the parked result; a no-op unless `has_more`.
    pub fn pull(&mut self) {
        if let (true, Some(r)) = (self.has_more(), &self.runner) {
            r.send(Command::Pull {
                run_id: self.run_id,
            });
            self.pulling = true;
        }
    }

    /// Stops the running statement. At a confirmation the question is withdrawn and the run
    /// gets a `Stop`, which skips that statement and ends the script; no cancel fires, so none
    /// stays armed. A parked stream gets a `Stop` too. A read-only one also gets the cancel at
    /// once, freeing a read-ahead blocked on slow rows; a read-write one drains so a write in
    /// the result commits, and only a second Cancel fires the handle (settle spends it).
    /// A run still connecting gets a `Stop`, so it starts no statement.
    /// A table still being described stops loading instead: its catalog read is interrupted
    /// and its answer dropped. With nothing executing, draining or loading, it does nothing.
    pub fn cancel(&mut self) {
        if self.target_pending.take().is_some() {
            // A target held behind a run has no read yet; the run is already being stopped.
            if !std::mem::take(&mut self.target_queued) {
                self.fire_cancel();
            }
            self.target_filter = None;
            self.target_sort = None;
            return;
        }
        self.stop_run();
    }

    // The run half of `cancel`.
    fn stop_run(&mut self) {
        // Withdrawing the question at once keeps the dialog's late reply from answering it.
        if let Some(r) = &self.runner
            && let Some(p) = self.confirm.take()
        {
            r.send(Command::Stop { run_id: p.run_id });
            self.withdrawn = true;
            return;
        }
        match (&self.confirm, &self.runner) {
            (None, Some(r))
                if self.running
                    && (self.paused.is_some() || self.parked_at_cap().is_some())
                    && !self.stopping =>
            {
                r.send(Command::Stop {
                    run_id: self.run_id,
                });
                if self.read_only == Some(true) {
                    self.fire_cancel();
                } else {
                    self.stopping = true;
                }
            }
            // A run still connecting has no handle yet, so the Stop ends it before its first
            // statement. Should the connect land first, a later Cancel fires the new handle.
            (None, Some(r)) if self.running && !self.withdrawn && self.cancel.is_none() => {
                r.send(Command::Stop {
                    run_id: self.run_id,
                });
            }
            // Only an executing statement or a drain has anything to cancel; an idle fire
            // would leave a Postgres cancel armed against the next statement.
            _ if (self.running && !self.withdrawn) || self.settling => self.fire_cancel(),
            _ => {}
        }
    }

    /// Cancels any statement, asks the runner to close its session and drops the runner and
    /// its pump; later runs do nothing. The returned `Closing` resolves once the runner has
    /// ended; dropping it leaves the runner to finish on its own.
    pub fn close(&mut self, cx: &mut Context<Self>) -> Closing {
        self.abort_save(cx);
        let closing = self.retire_runner(cx);
        self.closed = true;
        self.lost_settings = false;
        closing
    }

    /// Ends the runner as `close` does but keeps the model open: `link` reads Idle and the
    /// next `run` starts a new runner. The returned `Closing` resolves once the old one ended.
    pub fn disconnect(&mut self, cx: &mut Context<Self>) -> Closing {
        self.abort_save(cx);
        let closing = self.retire_runner(cx);
        self.lost_settings |= std::mem::take(&mut self.settings_changed);
        self.read_only = None;
        self.read_only_level = None;
        self.error = None;
        closing
    }

    /// Ends the current runner the way `close` does (cancelling any statement, ending unfinished
    /// ones as closed), switches `spec.workspace` to `workspace`, and connects a new runner. The
    /// model stays open. The returned `Closing` is the old runner's and resolves once it has ended.
    /// A closed model stays closed and returns an already resolved `Closing`.
    pub fn reconnect(&mut self, workspace: Arc<Workspace>, cx: &mut Context<Self>) -> Closing {
        if self.closed {
            return Closing(None);
        }
        let closing = self.retire_runner(cx);
        self.lost_settings |= std::mem::take(&mut self.settings_changed);
        self.spec.workspace = workspace;
        self.epoch += 1;
        // Unknown until the new session reports whether it opened read-only.
        self.read_only = None;
        self.read_only_level = None;
        self.start_runner(cx);
        if let Some(r) = &self.runner {
            r.send(Command::Connect);
        }
        closing
    }

    /// Retires the runner as `reconnect` does (cancelling a run, keeping SQL and results until
    /// the next run), points the spec at `name` in `workspace`, the current dbento.toml, and
    /// connects a new runner.
    pub fn switch_connection(
        &mut self,
        name: &str,
        workspace: Arc<Workspace>,
        cx: &mut Context<Self>,
    ) -> Result<Closing, SwitchBlocked> {
        if self.closed {
            return Err(SwitchBlocked::Closed);
        }
        if self.spec.connection == name {
            return Err(SwitchBlocked::Same);
        }
        if workspace.connection(name).is_none() {
            return Err(SwitchBlocked::Unknown);
        }
        if self.browses() {
            return Err(SwitchBlocked::Browse);
        }
        let closing = self.retire_runner(cx);
        self.spec.workspace = workspace;
        self.spec.connection = name.to_owned();
        self.epoch += 1;
        // A different connection never had the old session's settings to lose.
        self.lost_settings = false;
        // The old connection's link and error say nothing about the new one.
        self.read_only = None;
        self.read_only_level = None;
        self.error = None;
        self.start_runner(cx);
        if let Some(r) = &self.runner {
            r.send(Command::Connect);
        }
        Ok(closing)
    }

    /// A browse state, or a table tab still loading its target.
    pub fn browses(&self) -> bool {
        self.browse.is_some() || self.target_pending.is_some() || self.deferred_target.is_some()
    }

    // Asks the runner to close and drops it with its pump, resetting the run's state; the
    // results stay on screen. Shared by `close`, `reconnect` and `switch_connection`.
    fn retire_runner(&mut self, cx: &mut Context<Self>) -> Closing {
        if self.confirm.is_none() {
            self.fire_cancel();
        }
        let events = self.runner.take().map(|r| {
            r.send(Command::Close);
            r.events()
        });
        self.pump = None;
        self.cancel = None;
        self.connected = false;
        self.current_database = None;
        self.running = false;
        self.confirm = None;
        self.paused = None;
        self.pulling = false;
        self.stopping = false;
        self.settling = false;
        // No event reaches a retired runner's model, so an unfinished statement ends here.
        self.finish_unfinished(StopReason::Closed);
        self.append = None;
        self.ticking = None;
        self.flush_history(cx);
        cx.notify();
        Closing(events)
    }

    /// Applies one runner event; events from an earlier run are dropped, except `Settling`,
    /// whose drain belongs to whichever run left rows behind.
    pub fn apply(&mut self, event: Event) {
        self.settling = matches!(event, Event::Settling { .. });
        match event {
            Event::Connected {
                read_only,
                level,
                cancel,
                ..
            } => {
                self.read_only = Some(read_only);
                self.read_only_level = level;
                self.connected = true;
                self.settings_changed = false;
                self.current_database = None;
                self.cancel = Some(cancel);
                self.error = None;
            }
            Event::ConnectFailed { message } => self.error = Some(message),
            Event::SettingsChanged { database } => {
                self.settings_changed = true;
                self.current_database = database;
            }
            // The runner already refused this run for the session it lost, so the tab does not
            // refuse the next one again.
            Event::SettingsLost { run_id } if run_id == self.run_id => {
                self.settings_changed = false;
                self.notice = Some(runner::SETTINGS_LOST.into());
            }
            Event::Stopped { message } => {
                // The pump stays until the next run replaces it; dropping it here would
                // drop the task that is applying this event.
                self.runner = None;
                self.lost_settings |= std::mem::take(&mut self.settings_changed);
                self.cancel = None;
                self.connected = false;
                self.current_database = None;
                self.running = false;
                self.confirm = None;
                self.paused = None;
                self.pulling = false;
                self.stopping = false;
                // A pending target will never be answered; parking the message ends the wait.
                self.target_queued = false;
                if self.target_pending.take().is_some() {
                    self.target_landed = Some(Err(message.clone()));
                }
                self.error = Some(message);
                // No statement of a dead runner ends by itself, so none may stay "running".
                self.finish_unfinished(StopReason::Lost);
            }
            // A read-only session closing to end a cut result is routine, not news.
            Event::Disconnected { reason } => {
                self.cancel = None;
                self.connected = false;
                self.current_database = None;
                if self.read_only != Some(true) {
                    self.notice = Some(reason);
                }
            }
            Event::SettleFailed { message } => {
                // A failed commit after the run must not leave its last statement shown as Ok.
                if self.append.is_none()
                    && let Some(last) = self.results.iter().max_by_key(|r| r.stmt)
                    && last.plan.is_none()
                {
                    let entry = self.history_entry(
                        last.stmt,
                        &last.sql,
                        Duration::ZERO,
                        HistoryOutcome::Failed {
                            message: format!("after the statement: {message}"),
                        },
                    );
                    self.record(entry);
                }
                self.notice = Some(message);
            }
            // A continuation fills to its cap like Fetch next, so its first-rows park is pulled.
            Event::Paused { run_id, stmt } if run_id == self.run_id => {
                self.paused = Some(stmt);
                if self.append.as_ref().is_some_and(|a| !a.drifted) {
                    self.pull();
                }
            }
            // At its cap a stream is no longer pulled by scrolling; it waits for a Fetch.
            Event::ParkedAtCap {
                run_id,
                stmt,
                outcome,
                elapsed,
            } if run_id == self.run_id => {
                self.paused = None;
                self.pulling = false;
                // A re-run stopped at its cap has not loaded every matching row.
                self.kept_check.clear();
                // A continuation's runner counts only its own rows; result 0 holds them all.
                let mut outcome = outcome;
                if let Some(append) = &mut self.append {
                    outcome.rows = self.results.first().map_or(0, |r| r.store.rows());
                    append.outcome = outcome.clone();
                }
                self.cap = Some(Cap {
                    stmt,
                    outcome,
                    fetching: false,
                    ended: false,
                    expired: None,
                });
                self.ticking = None;
                let elapsed = self.browse_elapsed(elapsed);
                self.entry(stmt).elapsed = Some(elapsed);
            }
            Event::ParkExpired {
                run_id,
                statement_timeout,
            } if run_id == self.run_id => {
                if let Some(cap) = &mut self.cap {
                    cap.expired = Some(if statement_timeout {
                        Expiry::StatementTimeout
                    } else {
                        Expiry::Idle
                    });
                }
            }
            Event::ParkReleased { run_id } if run_id == self.run_id => {
                if let Some(cap) = &mut self.cap {
                    cap.expired = Some(Expiry::Released);
                }
            }
            // A continuation keeps result 0's text and store; its SQL is only how it resumes.
            Event::Executing { run_id, stmt, sql }
                if run_id == self.run_id && self.append.is_none() =>
            {
                self.mark_started(stmt);
                // A plan result names the user's statement, not the EXPLAIN text sent.
                let entry = self.entry(stmt);
                if entry.plan.is_none() {
                    entry.sql = sql;
                }
            }
            // A continuation appends by position, so one whose columns differ lands nothing.
            Event::StatementStarted {
                run_id, columns, ..
            } if run_id == self.run_id && self.append.is_some() => {
                let loaded = self.results.first().map(|r| r.store.columns());
                if !loaded.is_some_and(|l| same_columns(l, &columns))
                    && let Some(append) = &mut self.append
                {
                    append.drifted = true;
                    if let Some(r) = &self.runner {
                        r.send(Command::Stop { run_id });
                    }
                }
            }
            Event::StatementStarted {
                run_id,
                stmt,
                sql,
                columns,
            } if run_id == self.run_id => {
                self.mark_started(stmt);
                let entry = self.entry(stmt);
                entry.sql = sql;
                entry.store = ResultStore::new(columns);
                // A result with other column names drops its slot's view for good.
                if let Some(ix) = self.results.iter().position(|r| r.stmt == stmt)
                    && self.column_view(ix).is_none()
                {
                    self.column_views.remove(&ix);
                }
            }
            Event::NeedsConfirm {
                run_id,
                stmt,
                sql,
                kinds,
                analyze,
            } if run_id == self.run_id => {
                self.mark_started(stmt);
                let entry = self.entry(stmt);
                entry.sql = sql.clone();
                entry.store = ResultStore::new(Arc::new([]));
                self.confirm = Some(PendingConfirm {
                    run_id,
                    stmt,
                    sql,
                    kinds,
                    analyze,
                });
            }
            // An explain is statement 0 and sends no StatementDone when it gets an answer.
            Event::Planned {
                run_id,
                result,
                elapsed,
            } if run_id == self.run_id => {
                self.ticking = None;
                let entry = self.entry(0);
                let end = match result {
                    Ok(output) => {
                        if let Some(plan) = &mut entry.plan {
                            plan.output = Some(Arc::new(output));
                        }
                        StatementEnd::Planned
                    }
                    Err(ExecuteError::Cancelled) => StatementEnd::Stopped {
                        rows: 0,
                        reason: StopReason::User,
                    },
                    Err(e) => StatementEnd::Failed(e),
                };
                entry.store.finish(end);
                entry.elapsed = Some(elapsed);
            }
            Event::Batch {
                run_id,
                stmt,
                batch,
            } if run_id == self.run_id => {
                self.pulling = false;
                let mut batch = batch;
                if let Some(append) = &mut self.append {
                    // A sorted re-run repeats the loaded rows first; only those past them land.
                    let left = append.skip - append.dropped;
                    let len = batch.len() as u64;
                    append.dropped += left.min(len);
                    if left > 0 {
                        batch = batch.skip(left.min(len) as usize);
                    }
                }
                match &self.append {
                    Some(a) if a.drifted => {}
                    Some(_) => {
                        self.entry(stmt).store.push(batch);
                        self.pull();
                    }
                    None => self.entry(stmt).store.push(batch),
                }
            }
            Event::StatementDone {
                run_id,
                stmt,
                end,
                elapsed,
            } if run_id == self.run_id => {
                self.pulling = false;
                self.paused = None;
                if let Some(cap) = self.cap.as_mut().filter(|c| c.stmt == stmt) {
                    cap.ended = true;
                    cap.fetching = false;
                }
                self.stopping = false;
                let end = self.as_user_stop(stmt, end);
                let end = match self.append.is_some() {
                    true => self.appended_end(stmt, end),
                    false => {
                        let entry = self.entry(stmt);
                        if entry.plan.is_none() {
                            let sql = entry.sql.clone();
                            let outcome = history_outcome(&end);
                            let record = self.history_entry(stmt, &sql, elapsed, outcome);
                            self.record(record);
                        }
                        end
                    }
                };
                let complete = matches!(end, StatementEnd::Complete(_));
                self.ticking = None;
                let elapsed = self.browse_elapsed(elapsed);
                let entry = self.entry(stmt);
                entry.store.finish(end);
                entry.elapsed = Some(elapsed);
                if stmt == 0 {
                    self.check_kept(complete);
                }
                // `cancel` and `answer` withdraw the question themselves; this clears one
                // whose statement ended some other way.
                if self.confirm.as_ref().is_some_and(|p| p.stmt == stmt) {
                    self.confirm = None;
                }
            }
            Event::RunDone { run_id } if run_id == self.run_id => {
                self.running = false;
                self.kept_check.clear();
                self.confirm = None;
            }
            // Only the answer the tab still waits for counts; a cleared wait drops it.
            Event::Target {
                rel,
                result,
                foreign_keys,
            } if self.target_pending.as_ref() == Some(&rel) => {
                self.target_pending = None;
                self.target_landed = Some(result.map(|t| (t, foreign_keys)));
            }
            Event::Executing { .. }
            | Event::StatementStarted { .. }
            | Event::NeedsConfirm { .. }
            | Event::Planned { .. }
            | Event::Batch { .. }
            | Event::Paused { .. }
            | Event::ParkedAtCap { .. }
            | Event::ParkExpired { .. }
            | Event::ParkReleased { .. }
            | Event::StatementDone { .. }
            | Event::RunDone { .. }
            | Event::SettingsLost { .. }
            | Event::Target { .. }
            | Event::Settling { .. } => {}
        }
    }

    /// Asks the runner which column of `rel` each of `columns` reads; the answer comes on the
    /// returned channel. Err while a run holds the session, which answers only between runs.
    pub fn source_columns(
        &mut self,
        rel: RelationRef,
        columns: Arc<[ColumnInfo]>,
        cx: &mut Context<Self>,
    ) -> Result<Receiver<SourceColumns>, String> {
        if self.closed {
            return Err("the tab is closed".to_owned());
        }
        if self.running {
            return Err("wait for the run to finish".to_owned());
        }
        if self.runner.is_none() {
            self.start_runner(cx);
        }
        let (reply, answer) = async_channel::bounded(1);
        if let Some(r) = &self.runner {
            r.send(Command::SourceColumns {
                rel,
                columns,
                reply,
            });
        }
        Ok(answer)
    }

    /// Shows the value of Redis key `key` in DB `db` as result 0, reading its first page now
    /// and later pages on `load_more_value`.
    pub fn open_key(&mut self, db: u32, key: Vec<u8>, cx: &mut Context<Self>) {
        if self.closed {
            return;
        }
        self.key_task = None;
        self.results.clear();
        self.key_view = Some(KeyView::new(db, key));
        self.read_value(cx);
        cx.notify();
    }

    /// Asks for the value's next page; false when the value is read to its end, a page is
    /// already in flight, or a run holds the session.
    pub fn load_more_value(&mut self, cx: &mut Context<Self>) -> bool {
        let more = self.key_view.as_ref().is_some_and(KeyView::has_more);
        more && self.read_value(cx)
    }

    /// The key whose value this tab shows, if any.
    pub fn key_view(&self) -> Option<&KeyView> {
        self.key_view.as_ref()
    }

    // Sends the key view's next read to the runner and lands its answer in result 0.
    fn read_value(&mut self, cx: &mut Context<Self>) -> bool {
        if self.closed || self.running {
            return false;
        }
        let Some(read) = self.key_view.as_ref().and_then(KeyView::read) else {
            return false;
        };
        if self.runner.is_none() {
            self.start_runner(cx);
        }
        let Some(r) = &self.runner else {
            return false;
        };
        let (reply, answer) = async_channel::bounded(1);
        r.send(Command::ReadKey { read, reply });
        if let Some(view) = &mut self.key_view {
            view.begin();
        }
        self.key_task = Some(cx.spawn(async move |this, cx| {
            // The runner drops a read that meets a run, and a retired runner drops it too.
            let answer =
                (answer.recv().await).unwrap_or_else(|_| Err("the session stopped".into()));
            let _ = this.update(cx, |m, cx| {
                m.key_task = None;
                m.value_landed(answer);
                cx.notify();
            });
        }));
        cx.notify();
        true
    }

    // A first page, or one whose kind differs from the last because the read restarted on the
    // key's new kind, starts result 0 afresh; any other page appends to it.
    fn value_landed(&mut self, answer: KeyValueAnswer) {
        let Some(view) = &mut self.key_view else {
            return;
        };
        let page = match answer {
            Ok(page) => page,
            Err(e) => return view.fail(e),
        };
        let restart = view.kind() != Some(&page.kind);
        let (columns, batch) = view.apply(page);
        let sql = view.title();
        if restart {
            self.results.clear();
        }
        if self.results.is_empty() {
            self.results.push(StatementResult {
                stmt: 0,
                sql,
                engine: Some(Engine::Redis),
                epoch: self.epoch,
                store: ResultStore::new(columns.into()),
                elapsed: None,
                source: None,
                plan: None,
            });
        }
        let store = &mut self.results[0].store;
        store.push(batch);
        if !view.has_more() {
            let rows = store.rows();
            store.finish(StatementEnd::Complete(Outcome::complete(rows)));
        }
    }

    /// Starts the runner if needed and asks it for `rel`'s target; the tab browses once it lands.
    pub fn load_target(&mut self, rel: RelationRef, cx: &mut Context<Self>) {
        self.load_target_filtered(rel, None, cx);
    }

    /// `load_target` whose first browse state carries `filter`.
    pub fn load_target_filtered(
        &mut self,
        rel: RelationRef,
        filter: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.load_target_browse(rel, Browse::new(filter, None), cx);
    }

    /// `load_target` whose first browse state carries `browse`'s filter and sort.
    pub fn load_target_browse(&mut self, rel: RelationRef, browse: Browse, cx: &mut Context<Self>) {
        if self.closed {
            return;
        }
        // A run drops a Target, so the target replaces the run as a new Run would: the run is
        // stopped as Cancel stops it, and the Target goes out once it has ended.
        let queued = self.running;
        if queued {
            // A cancel landing between statements would let the script run on; a Stop ends
            // it before the next one. Every other state gets its Stop from `stop_run`.
            let executing = self.running() && self.confirm.is_none() && !self.withdrawn;
            if let Some(r) = self
                .runner
                .as_ref()
                .filter(|_| executing && self.cancel.is_some())
            {
                r.send(Command::Stop {
                    run_id: self.run_id,
                });
            }
            self.stop_run();
        }
        if self.runner.is_none() {
            // The new target replaces any pending one, so a new runner need not ask for both.
            self.target_pending = None;
            self.start_runner(cx);
        }
        if let Some(r) = self.runner.as_ref().filter(|_| !queued) {
            r.send(Command::Target { rel: rel.clone() });
        }
        self.target_queued = queued;
        self.target_landed = None;
        self.target_pending = Some(rel);
        self.target_filter = browse.filter;
        self.target_sort = browse.sort;
        self.deferred_target = None;
        cx.notify();
    }

    /// Holds `rel` and `browse` so the next run loads the target and browses instead of
    /// running the editor's text: a restored table tab runs nothing until asked.
    pub fn defer_target(&mut self, rel: RelationRef, browse: Browse, cx: &mut Context<Self>) {
        self.deferred_target = Some((rel, browse));
        cx.notify();
    }

    /// The restored table the next run browses, if any.
    pub fn deferred_target(&self) -> Option<&RelationRef> {
        self.deferred_target.as_ref().map(|(rel, _)| rel)
    }

    /// The filter and sort this tab browses with, whether browsing, waiting for its target or
    /// deferred; None when it does not browse.
    pub fn browse_settings(&self) -> Option<Browse> {
        if let Some(state) = &self.browse {
            let b = state.browse();
            return Some(Browse::new(b.filter.clone(), b.sort.clone()));
        }
        if self.target_pending.is_some() {
            return Some(Browse::new(
                self.target_filter.clone(),
                self.target_sort.clone(),
            ));
        }
        self.deferred_target.as_ref().map(|(_, b)| b.clone())
    }

    /// The relation whose description has been asked for and not yet landed.
    pub fn target_pending(&self) -> Option<&RelationRef> {
        self.target_pending.as_ref()
    }

    /// The table this tab browses, if any.
    pub fn browse(&self) -> Option<&BrowseState> {
        self.browse.as_ref()
    }

    /// Stores `state`, clears the filter error, emits `BrowseEvent::Run(state.sql())`.
    pub fn set_browse(&mut self, state: BrowseState, cx: &mut Context<Self>) {
        let sql = state.sql().to_owned();
        self.browse = Some(state);
        self.filter_error = None;
        cx.emit(BrowseEvent::Run(sql));
        cx.notify();
    }

    /// Drops the browse state, any pending target and the filter error.
    pub fn clear_browse(&mut self, cx: &mut Context<Self>) {
        // The editor calls this on every keystroke, so an idle clear stays silent.
        let browsed = self.browses();
        let changed = self.abort_save(cx)
            | self.browse.take().is_some()
            | self.target_pending.take().is_some()
            | self.deferred_target.take().is_some()
            | self.target_filter.take().is_some()
            | self.target_sort.take().is_some()
            | self.target_landed.take().is_some()
            | self.filter_error.take().is_some()
            | !self.edits.is_empty()
            | self.banner.take().is_some()
            | self.discard.take().is_some();
        self.edits.clear();
        self.edit_history.clear();
        self.stale = false;
        self.announce_fresh = false;
        if browsed {
            cx.emit(BrowseEvent::Cleared);
        }
        if changed {
            cx.notify();
        }
    }

    /// `with_filter` then `set_browse`; on Err stores the message as `filter_error`. A valid
    /// filter with edits staged asks `Discard::Filter` instead. False without a browse state,
    /// on Err, or when it asks.
    pub fn apply_filter(&mut self, filter: &str, cx: &mut Context<Self>) -> bool {
        let Some(state) = &self.browse else {
            return false;
        };
        match state.with_filter(filter) {
            Ok(next) => self.guard(Discard::Filter(filter.to_owned()), next, cx),
            Err(e) => {
                self.filter_error = Some(e.to_string());
                cx.notify();
                false
            }
        }
    }

    /// `with_next_sort` then `set_browse`; a valid sort with edits staged asks
    /// `Discard::Sort` instead. False without a browse state, on Err, or when it asks.
    pub fn cycle_sort(&mut self, column: &str, cx: &mut Context<Self>) -> bool {
        match self.browse.as_ref().map(|s| s.with_next_sort(column)) {
            Some(Ok(next)) => self.guard(Discard::Sort(column.to_owned()), next, cx),
            _ => false,
        }
    }

    /// The browse sort set outright, through the discard guard. False without a browse, on an
    /// unknown or unsortable column, when unchanged, or when it asks.
    pub fn set_sort(&mut self, sort: Option<Sort>, cx: &mut Context<Self>) -> bool {
        let Some(state) = &self.browse else {
            return false;
        };
        if state.browse().sort == sort {
            return false;
        }
        match state.with_sort(sort.clone()) {
            Ok(next) => self.guard(Discard::SetSort(sort), next, cx),
            Err(_) => false,
        }
    }

    /// Sorts statement `stmt`'s loaded rows by data column `col` (Some(descending)) or restores
    /// loaded order (None). False for statement 0 of a browse result, out of range, or no change.
    pub fn sort_loaded(
        &mut self,
        stmt: usize,
        col: usize,
        sort: Option<bool>,
        cx: &mut Context<Self>,
    ) -> bool {
        if stmt == 0 && self.browse_result() {
            return false;
        }
        let Some(store) = self.results.get_mut(stmt).map(|r| &mut r.store) else {
            return false;
        };
        if col >= store.columns().len() || store.local_sort() == sort.map(|d| (col, d)) {
            return false;
        }
        match sort {
            Some(descending) => store.sort_local(col, descending),
            None => store.clear_local_sort(),
        }
        cx.notify();
        true
    }

    /// Applies a validated browse change at once with nothing staged; otherwise asks.
    fn guard(&mut self, action: Discard, next: BrowseState, cx: &mut Context<Self>) -> bool {
        if self.ask_discard(action, cx) {
            self.set_browse(next, cx);
            true
        } else {
            false
        }
    }

    /// True when nothing is staged, so a guarded action may go ahead (a question left over
    /// from edits since dropped goes with it). Otherwise records the question, unless one is
    /// already pending, and returns false.
    fn ask_discard(&mut self, action: Discard, cx: &mut Context<Self>) -> bool {
        if self.edits.is_empty() {
            if self.discard.take().is_some() {
                cx.notify();
            }
            return true;
        }
        if self.discard.is_none() {
            self.discards += 1;
            self.discard = Some(PendingDiscard {
                id: self.discards,
                title: staged::discard_title(self.edits.len()),
                action,
            });
            cx.notify();
        }
        false
    }

    /// Runs `sql` at once (true) with nothing staged; otherwise asks (false).
    pub fn request_run(&mut self, sql: String, cx: &mut Context<Self>) -> bool {
        self.request_run_at(sql, 0, cx)
    }

    /// `request_run` for text that begins at editor offset `start`; Discard runs it there.
    /// A restored table tab's first run loads its target instead, browsing as it was left.
    pub fn request_run_at(&mut self, sql: String, start: usize, cx: &mut Context<Self>) -> bool {
        if let Some((rel, browse)) = self.deferred_target.take() {
            self.load_target_browse(rel, browse, cx);
            return true;
        }
        let asked = self.discard.is_some();
        if self.ask_discard(Discard::Run(sql.clone()), cx) {
            self.run_at(sql, start, cx);
            true
        } else {
            if !asked {
                self.discard_start = start;
            }
            false
        }
    }

    /// `clear_browse` at once (true) with nothing staged; otherwise asks (false).
    pub fn request_clear_browse(&mut self, cx: &mut Context<Self>) -> bool {
        if self.ask_discard(Discard::Edit, cx) {
            self.clear_browse(cx);
            true
        } else {
            false
        }
    }

    /// The discard question waiting on an answer.
    pub fn discard_question(&self) -> Option<&PendingDiscard> {
        self.discard.as_ref()
    }

    /// Answers question `id`. Discard performs the action and, once it took effect, clears
    /// the edits, history and banner; Keep drops the question. Returns the action; None for
    /// a stale id.
    pub fn answer_discard(
        &mut self,
        id: u64,
        discard: bool,
        cx: &mut Context<Self>,
    ) -> Option<Discard> {
        if self.discard.as_ref().map(|q| q.id) != Some(id) {
            return None;
        }
        let action = self.discard.take()?.action;
        cx.notify();
        if !discard {
            return Some(action);
        }
        match &action {
            // `clear_browse` drops the edits, history and banner itself. A run's go first, so a
            // commit still settling does not refuse the run the user chose to discard for.
            Discard::Run(sql) => {
                self.edits.clear();
                self.edit_history.clear();
                self.banner = None;
                self.run_at(sql.clone(), self.discard_start, cx)
            }
            Discard::Explain(request) => {
                self.edits.clear();
                self.edit_history.clear();
                self.banner = None;
                self.explain_at(request.clone(), self.discard_start, cx)
            }
            Discard::Edit => self.clear_browse(cx),
            // Recomputed, since the question only held the request. The edits go before
            // `set_browse` emits its run, so that run does not ask again.
            Discard::Filter(_) | Discard::Sort(_) | Discard::SetSort(_) => {
                let next = match (&action, &self.browse) {
                    (Discard::Filter(f), Some(state)) => state.with_filter(f).map_err(Some),
                    (Discard::Sort(c), Some(state)) => state.with_next_sort(c).map_err(|_| None),
                    (Discard::SetSort(s), Some(state)) => {
                        state.with_sort(s.clone()).map_err(|_| None)
                    }
                    _ => return Some(action),
                };
                match next {
                    Ok(next) => {
                        self.edits.clear();
                        self.edit_history.clear();
                        self.banner = None;
                        self.set_browse(next, cx);
                    }
                    Err(Some(e)) => self.filter_error = Some(e.to_string()),
                    Err(None) => {}
                }
            }
        }
        Some(action)
    }

    /// Why the last filter was refused; cleared by the next applied browse state.
    pub fn filter_error(&self) -> Option<&str> {
        self.filter_error.as_deref()
    }

    /// `browse::edit_block` over the browse state, `read_only() == Some(true)` and the last
    /// run's text, then changed session settings, then a failed run; None without a browse
    /// state or before the first result of a run has ended.
    pub fn edit_block(&self) -> Option<EditBlock> {
        let state = self.browse.as_ref()?;
        if !self.results.iter().any(|r| r.store.end().is_some()) {
            return None;
        }
        let last_run = self.run_text.as_ref().map(|(_, text)| text.as_str());
        let settings = self.settings_changed.then_some(EditBlock::SettingsChanged);
        browse::edit_block(state, self.read_only == Some(true), last_run)
            .or(settings)
            .or_else(|| {
                self.results
                    .first()
                    .is_some_and(|r| r.store.end().is_some_and(failed_or_skipped))
                    .then_some(EditBlock::RunFailed)
            })
    }

    /// Browse state, read-write session with unchanged settings, exactly one result, the last
    /// run is the browse SQL, and that statement did not end Failed or Skipped. True while the
    /// stream is parked.
    pub fn editable(&self) -> bool {
        self.read_only == Some(false)
            && !self.settings_changed
            && self.browse_result()
            && self
                .browse
                .as_ref()
                .is_some_and(|s| s.target().has_identity())
    }

    /// A browse state, exactly one result, the last run's text is the browse SQL (trimmed),
    /// and that statement did not end Failed or Skipped. Read-only and identity do not matter.
    pub fn browse_result(&self) -> bool {
        let Some(state) = &self.browse else {
            return false;
        };
        let last_run = self.run_text.as_ref().map(|(_, text)| text.trim());
        self.results.len() == 1
            && last_run == Some(state.sql().trim())
            && !self.results[0].store.end().is_some_and(failed_or_skipped)
    }

    /// The staged edits of the browse result.
    pub fn edits(&self) -> &StagedEdits {
        &self.edits
    }

    /// `staged::grid_row(table_row, self.edits().inserted())`.
    pub fn grid_row(&self, table_row: usize) -> GridRow {
        staged::grid_row(table_row, self.edits.inserted())
    }

    /// The shown value of a cell Set NULL or Set DEFAULT may change: editable tab, save
    /// Idle, an editable target column (never a locator), and a live loaded row in the
    /// result (not deleted, a supported value kind) or an existing inserted row.
    pub fn settable(&self, at: GridRow, col: usize) -> Option<EditValue> {
        self.settable_cell(at, col).map(|cell| cell.shown)
    }

    /// In-cell editor text: single-line text as is, "" for NULL or DEFAULT; None for bytes,
    /// multi-line text, or a cell `settable` refuses.
    pub fn cell_text_at(&self, at: GridRow, col: usize) -> Option<String> {
        editor_text(&self.settable(at, col)?)
    }

    /// Stages typed text; false when `cell_text_at` is None. Empty text keeps a shown NULL
    /// or DEFAULT, is NULL over a loaded NULL, and is empty text otherwise.
    pub fn stage_text(
        &mut self,
        at: GridRow,
        col: usize,
        text: &str,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(cell) = self
            .settable_cell(at, col)
            .filter(|cell| editor_text(&cell.shown).is_some())
        else {
            return false;
        };
        let value = if !text.is_empty() {
            EditValue::Text(text.to_owned())
        } else {
            match (&cell.shown, &cell.loaded) {
                (EditValue::Null | EditValue::Default, _) => cell.shown.clone(),
                (_, Some((_, EditValue::Null))) => EditValue::Null,
                _ => EditValue::Text(String::new()),
            }
        };
        self.stage_settable(at, col, cell, value, cx)
    }

    /// Stages `value` on a `settable` cell; false otherwise.
    pub fn set_value(
        &mut self,
        at: GridRow,
        col: usize,
        value: EditValue,
        cx: &mut Context<Self>,
    ) -> bool {
        let Some(cell) = self.settable_cell(at, col) else {
            return false;
        };
        self.stage_settable(at, col, cell, value, cx)
    }

    /// An empty inserted row; its index. None unless editable with the save Idle.
    pub fn add_row(&mut self, cx: &mut Context<Self>) -> Option<usize> {
        if !self.can_edit_rows() {
            return None;
        }
        let ix = self.track(|edits| edits.add_row(Vec::new()));
        cx.notify();
        Some(ix)
    }

    /// An inserted copy of `at`: the shown value of every editable column, with primary-key
    /// columns and values of an unsupported kind left DEFAULT; its index. A deleted loaded
    /// row may be copied.
    pub fn duplicate_row(&mut self, at: GridRow, cx: &mut Context<Self>) -> Option<usize> {
        if !self.can_edit_rows() {
            return None;
        }
        let store = &self.results[0].store;
        let target = self.browse.as_ref()?.target();
        match at {
            GridRow::Loaded(row) if row >= store.rows() => return None,
            GridRow::Inserted(ix) if ix >= self.edits.inserted() => return None,
            GridRow::Inserted(_) | GridRow::Loaded(_) => {}
        }
        let mut cells = Vec::new();
        for (col, info) in store.columns().iter().enumerate() {
            let Some(column) = target.column(info.name()) else {
                continue;
            };
            if !column.editable() || column.meta().primary_key().is_some() {
                continue;
            }
            let shown = match at {
                GridRow::Loaded(row) => self
                    .edits
                    .cell(row, col)
                    .cloned()
                    .or_else(|| staged::edit_value(store.cell(row, col)?)),
                GridRow::Inserted(ix) => self.edits.new_cell(ix, col).cloned(),
            };
            let value = shown.unwrap_or(EditValue::Default);
            cells.push((col, info.name().to_owned(), value));
        }
        let ix = self.track(|edits| edits.add_row(cells));
        cx.notify();
        Some(ix)
    }

    /// Marks a loaded row deleted, dropping its cell edits, or removes an inserted row;
    /// false when the row cannot be edited.
    pub fn delete_at(&mut self, at: GridRow, cx: &mut Context<Self>) -> bool {
        if !self.can_edit_rows() {
            return false;
        }
        let deleted = match at {
            GridRow::Inserted(ix) => self.track(|edits| edits.remove_new(ix)),
            GridRow::Loaded(row) => {
                if row >= self.results[0].store.rows() || self.edits.is_deleted(row) {
                    return false;
                }
                let Some(key) = self.row_key(row) else {
                    return false;
                };
                self.track(|edits| edits.delete(row, key));
                true
            }
        };
        cx.notify();
        deleted
    }

    /// Drops one row's change (an inserted row is removed); false when it has none or a
    /// save is in flight.
    pub fn revert_row(&mut self, at: GridRow, cx: &mut Context<Self>) -> bool {
        if self.save != SaveState::Idle {
            return false;
        }
        let reverted = self.track(|edits| match at {
            GridRow::Inserted(ix) => edits.remove_new(ix),
            GridRow::Loaded(row) => edits.revert_row(row),
        });
        self.forget_reverted();
        cx.notify();
        reverted
    }

    /// The loaded value behind a browse cell, from the result store; None for inserted rows
    /// and out of range.
    pub fn original_at(&self, at: GridRow, col: usize) -> Option<CellRef<'_>> {
        match at {
            GridRow::Loaded(row) if self.browse_result() => self.results[0].store.cell(row, col),
            GridRow::Inserted(_) | GridRow::Loaded(_) => None,
        }
    }

    /// `StagedEdits::revert_cell` as one undo step; false for inserted rows, unstaged cells
    /// or while saving.
    pub fn revert_cell(&mut self, at: GridRow, col: usize, cx: &mut Context<Self>) -> bool {
        let GridRow::Loaded(row) = at else {
            return false;
        };
        if self.save != SaveState::Idle {
            return false;
        }
        let reverted = self.track(|edits| edits.revert_cell(row, col));
        if reverted {
            self.forget_reverted();
            cx.notify();
        }
        reverted
    }

    /// Whether `at` has a staged change (an inserted row always has).
    pub fn has_change(&self, at: GridRow) -> bool {
        match at {
            GridRow::Inserted(ix) => ix < self.edits.inserted(),
            GridRow::Loaded(row) => self.edits.row(row).is_some(),
        }
    }

    /// `render_sql` of one change built from row `at`: Insert from every result column that
    /// is a target column, with its shown value; Update sets the same, keyed by the loaded
    /// row's identity; Delete keyed by it. None outside a browse result, for Update or Delete
    /// on an inserted row or without an identity, for a value of an unsupported kind, or when
    /// `EditBatch::new` refuses the change.
    pub fn copy_query(&self, at: GridRow, kind: CopyKind) -> Option<String> {
        if !self.browse_result() {
            return None;
        }
        let target = self.browse.as_ref()?.target();
        let store = &self.results[0].store;
        match at {
            GridRow::Loaded(row) if row >= store.rows() => return None,
            GridRow::Inserted(ix) if ix >= self.edits.inserted() => return None,
            GridRow::Inserted(_) | GridRow::Loaded(_) => {}
        }
        // Shown values are the staged cell over the loaded one; an inserted row's unset
        // cells stay out, since an INSERT leaves them to their default anyway.
        let mut values = Vec::new();
        for (col, info) in store.columns().iter().enumerate() {
            if target.column(info.name()).is_none() {
                continue;
            }
            let shown = match at {
                GridRow::Loaded(row) => match self.edits.cell(row, col) {
                    Some(staged) => staged.clone(),
                    None => staged::edit_value(store.cell(row, col)?)?,
                },
                GridRow::Inserted(ix) => match self.edits.new_cell(ix, col) {
                    Some(value) => value.clone(),
                    None => continue,
                },
            };
            values.push((info.name().to_owned(), shown));
        }
        let key = || match at {
            GridRow::Loaded(row) => self.row_key(row),
            GridRow::Inserted(_) => None,
        };
        let change = match kind {
            CopyKind::Insert => Change::Insert { values },
            CopyKind::Update => Change::Update {
                key: key()?,
                set: values,
            },
            CopyKind::Delete => Change::Delete { key: key()? },
        };
        let batch = EditBatch::new(target.clone(), vec![change]).ok()?;
        Some(render_sql(&batch))
    }

    /// `cell_text_at(GridRow::Loaded(row), col)`.
    pub fn cell_edit_text(&self, row: u64, col: usize) -> Option<String> {
        self.cell_text_at(GridRow::Loaded(row), col)
    }

    /// `stage_text(GridRow::Loaded(row), ..)`.
    pub fn stage_cell(&mut self, row: u64, col: usize, text: &str, cx: &mut Context<Self>) -> bool {
        self.stage_text(GridRow::Loaded(row), col, text, cx)
    }

    /// `delete_at(GridRow::Loaded(row))`.
    pub fn delete_row(&mut self, row: u64, cx: &mut Context<Self>) -> bool {
        self.delete_at(GridRow::Loaded(row), cx)
    }

    /// Drops every staged edit, mark and the banner; does nothing while a save is in flight.
    pub fn revert_edits(&mut self, cx: &mut Context<Self>) {
        if self.save != SaveState::Idle {
            return;
        }
        self.track(StagedEdits::clear);
        self.banner = None;
        self.clear_conflicts();
        cx.notify();
    }

    /// Restores the staged set before the last change, dropping the banner once nothing is
    /// staged; false with no step or a save in flight.
    pub fn undo(&mut self, cx: &mut Context<Self>) -> bool {
        let undone = self.save == SaveState::Idle && self.edit_history.undo(&mut self.edits);
        self.stepped(undone, cx)
    }

    /// Restores the staged set the last undo replaced, dropping the banner once nothing is
    /// staged; false with no step or a save in flight.
    pub fn redo(&mut self, cx: &mut Context<Self>) -> bool {
        let redone = self.save == SaveState::Idle && self.edit_history.redo(&mut self.edits);
        self.stepped(redone, cx)
    }

    // A rejection banner describes edits that are staged; with none left it no longer applies.
    fn stepped(&mut self, moved: bool, cx: &mut Context<Self>) -> bool {
        if moved {
            self.forget_reverted();
            if self.edits.is_empty() {
                self.banner = None;
                self.clear_conflicts();
            }
            cx.notify();
        }
        moved
    }

    /// True when `undo` would restore a step.
    pub fn can_undo(&self) -> bool {
        self.save == SaveState::Idle && self.edit_history.can_undo()
    }

    /// True when `redo` would restore a step.
    pub fn can_redo(&self) -> bool {
        self.save == SaveState::Idle && self.edit_history.can_redo()
    }

    /// Applies `change` to the staged set, recording the state before it as an undo step
    /// only when the set actually changed.
    fn track<R>(&mut self, change: impl FnOnce(&mut StagedEdits) -> R) -> R {
        let before = self.edits.clone();
        let result = change(&mut self.edits);
        if self.edits != before {
            self.edit_history.record(before);
        }
        result
    }

    /// Something staged and `save_ready`.
    pub fn can_save(&self) -> bool {
        !self.edits.is_empty() && self.save_ready()
    }

    /// Editable, no save in flight, no question pending, and nothing executing: a running tab
    /// qualifies only while its stream is parked (paused or at its cap). Staged edits aside.
    pub fn save_ready(&self) -> bool {
        // The field, not `running()`: a parked stream still holds its read lock, so `save`
        // stops it first.
        self.editable()
            && !self.stale
            && self.save == SaveState::Idle
            && self.confirm.is_none()
            && (!self.running || self.paused.is_some() || self.parked_at_cap().is_some())
    }

    /// Applies the staged edits in one transaction on a fresh session, then holds it open
    /// behind `apply_prompt`; a rejected batch sets the banner and row marks. A parked stream
    /// is stopped first (`Stopping`). False when `can_save` is not.
    pub fn save(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.can_save() {
            return false;
        }
        self.banner = None;
        self.clear_conflicts();
        self.edits.clear_marks();
        self.edit_history.clear_marks();
        if self.running {
            // The parked stream's read lock would fail the COMMIT; `flush_save` applies once
            // its RunDone lands.
            self.stop_run();
            self.save = SaveState::Stopping;
        } else {
            self.start_apply(cx);
        }
        cx.notify();
        true
    }

    /// Starts the apply once a stopped stream's RunDone has landed; the pump calls it.
    pub fn flush_save(&mut self, cx: &mut Context<Self>) {
        if self.save == SaveState::Stopping && !self.running {
            self.start_apply(cx);
            cx.notify();
        }
    }

    /// Abandons a save in `Stopping`, `Applying` or `Prompt`: dropping the task aborts an
    /// apply in flight (its session rolls back), and a held transaction is rolled back on
    /// tokio. A save in `Finishing` ends on its own. True when a save was abandoned.
    fn abort_save(&mut self, cx: &mut Context<Self>) -> bool {
        if !matches!(
            self.save,
            SaveState::Stopping | SaveState::Applying | SaveState::Prompt
        ) {
            return false;
        }
        self.save_task = None;
        if let Some(applied) = self.applied.take() {
            tokio_bridge::spawn(cx, applied.rollback()).detach();
        }
        self.prompt = None;
        self.save = SaveState::Idle;
        true
    }

    /// Where the save stands.
    pub fn save_state(&self) -> SaveState {
        self.save
    }

    /// The question of an applied batch waiting for Commit or Roll back.
    pub fn apply_prompt(&self) -> Option<&ApplyPrompt> {
        self.prompt.as_ref()
    }

    /// Commits the open transaction; on success clears the edits and runs the browse again.
    pub fn commit(&mut self, cx: &mut Context<Self>) {
        self.finish(true, cx);
    }

    /// Rolls the open transaction back, keeping the edits staged.
    pub fn rollback(&mut self, cx: &mut Context<Self>) {
        self.finish(false, cx);
    }

    /// Why the last save committed nothing, or why its COMMIT failed.
    pub fn edit_banner(&self) -> Option<&str> {
        self.banner.as_deref()
    }

    /// Loaded rows whose change came back `ChangeOutcome::Conflict` in the last rejected save.
    pub fn conflicts(&self) -> &[GridRow] {
        &self.conflicts
    }

    /// Unstages the conflicting rows (one undo step), clears banner and marks, then `save`s
    /// the rest through the normal prompt. False without conflicts or while saving.
    pub fn drop_conflicts(&mut self, cx: &mut Context<Self>) -> bool {
        if self.conflicts.is_empty() || self.save != SaveState::Idle {
            return false;
        }
        // Conflicts always hold staged rows, so equal counts mean nothing else would be saved.
        if self.edits.len() <= self.conflicts.len() {
            self.notice = Some("Nothing left to save".into());
            cx.notify();
            return false;
        }
        let conflicts = std::mem::take(&mut self.conflicts);
        self.track(|edits| {
            for at in &conflicts {
                if let GridRow::Loaded(row) = at {
                    edits.revert_row(*row);
                }
            }
        });
        self.edits.clear_marks();
        self.banner = None;
        self.clear_conflicts();
        self.save(cx);
        cx.notify();
        true
    }

    /// Conflicts exist, the target has a stable key, nothing saves or reloads.
    pub fn can_reload_conflicts(&self) -> bool {
        !self.conflicts.is_empty()
            && self.reload_task.is_none()
            && self.save == SaveState::Idle
            && self
                .browse
                .as_ref()
                .is_some_and(|b| b.target().stable_key())
    }

    /// Reads the conflicting rows by key on a fresh session; staged edits stay. False unless
    /// allowed.
    pub fn reload_conflicts(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.can_reload_conflicts() {
            return false;
        }
        let Some(target) = self.browse.as_ref().map(|b| b.target().clone()) else {
            return false;
        };
        // Each conflict by the key it was staged under, so the read finds what holds it now.
        let keys: Vec<(u64, RowKey)> = self
            .conflicts
            .iter()
            .filter_map(|at| match at {
                GridRow::Loaded(row) => match self.edits.row(*row)? {
                    staged::RowEdit::Update { key, .. } | staged::RowEdit::Delete { key } => {
                        Some((*row, key.clone()))
                    }
                },
                GridRow::Inserted(_) => None,
            })
            .collect();
        if keys.is_empty() {
            return false;
        }
        let filter = keys
            .iter()
            .map(|(_, key)| {
                dbento_core::key_filter(target.engine(), key.values()).map(|f| format!("({f})"))
            })
            .collect::<Option<Vec<_>>>()
            .map(|terms| terms.join("\nOR "));
        let sql = filter.and_then(|f| browse_sql(&target, &Browse::new(Some(f), None)).ok());
        let Some(sql) = sql else {
            return false;
        };
        let spec = self.spec.clone();
        let max_rows = keys.len() as u64;
        // A fresh read-only session, never the tab's: the tab may hold a parked stream or a BEGIN.
        let work = tokio_bridge::spawn(cx, async move {
            let mut session =
                runner::open(&spec, |c| catalog_options(c.statement_timeout())).await?;
            let read = read_rows(&mut session, &sql, max_rows).await;
            // A read-only session leaves no write for close to report.
            let _ = session.close().await;
            read
        });
        self.reload_task = Some(cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |m, cx| {
                m.reload_task = None;
                match result {
                    Ok(Ok(store)) => {
                        m.reloaded(store, &keys);
                        if m.notice
                            .as_deref()
                            .is_some_and(|n| n.starts_with(RELOAD_FAILED))
                        {
                            m.notice = None;
                        }
                    }
                    Ok(Err(e)) => m.notice = Some(format!("{RELOAD_FAILED}: {e}")),
                    Err(_) => m.notice = Some(format!("{RELOAD_FAILED}: the read stopped")),
                }
                cx.notify();
            });
        }));
        cx.notify();
        true
    }

    /// Whether the conflicting rows' reload is in flight.
    pub fn reloading(&self) -> bool {
        self.reload_task.is_some()
    }

    /// What the last reload found for a conflicting row's cell; None for other rows or
    /// before a reload.
    pub fn fresh_at(&self, at: GridRow, col: usize) -> Option<staged::Fresh<'_>> {
        let GridRow::Loaded(row) = at else {
            return None;
        };
        let fresh = self.fresh.as_ref()?;
        match fresh.rows.get(&row)? {
            Some(r) => fresh.store.cell(*r, col).map(staged::Fresh::Value),
            None => Some(staged::Fresh::Gone),
        }
    }

    /// Whether the last reload found no row with `at`'s staged key.
    pub fn row_gone(&self, at: GridRow) -> bool {
        // A gone row reads Gone in every column, so column 0 stands for the row.
        self.fresh_at(at, 0) == Some(staged::Fresh::Gone)
    }

    // Each conflicting row is the fresh row with its staged key; none with it means it is gone.
    fn reloaded(&mut self, store: ResultStore, keys: &[(u64, RowKey)]) {
        let Some(identity) = self.browse.as_ref().map(|b| b.target().identity().to_vec()) else {
            return;
        };
        // Fresh values are read by the loaded column index, which only holds while the shape does.
        let loaded = self.results.first().map(|r| r.store.columns());
        if !loaded.is_some_and(|l| same_columns(l, store.columns())) {
            self.fresh = None;
            self.columns_changed();
            return;
        }
        let fresh_keys: Vec<Option<RowKey>> = (0..store.rows())
            .map(|r| {
                let cells = store
                    .columns()
                    .iter()
                    .enumerate()
                    .map(|(c, info)| Some((info.name(), store.cell(r, c)?)))
                    .collect::<Option<Vec<_>>>()?;
                staged::row_key(&identity, &cells)
            })
            .collect();
        let rows = keys
            .iter()
            .map(|(row, key)| {
                let found = fresh_keys.iter().position(|k| k.as_ref() == Some(key));
                (*row, found.map(|r| r as u64))
            })
            .collect();
        self.fresh = Some(FreshRows { store, rows });
    }

    /// Forgets conflicting rows no longer staged; once none is left, its reload and fresh
    /// values go, and the banner too when no other rejected row is still marked.
    fn forget_reverted(&mut self) {
        if self.conflicts.is_empty() {
            return;
        }
        let edits = &self.edits;
        self.conflicts.retain(|at| match at {
            GridRow::Loaded(row) => edits.row(*row).is_some(),
            GridRow::Inserted(_) => false,
        });
        if self.conflicts.is_empty() {
            self.clear_conflicts();
            if !self.edits.has_marks() {
                self.banner = None;
            }
        }
    }

    /// Drops the conflict list, any reload in flight and what a reload found.
    fn clear_conflicts(&mut self) {
        self.conflicts.clear();
        self.reload_task = None;
        self.fresh = None;
    }

    /// Committed updated rows missing from the untruncated post-commit re-run; the next run
    /// clears them.
    pub fn kept_rows(&self) -> &[KeptRow] {
        &self.kept
    }

    /// Each updated loaded row of the batch in flight, when its target's keys survive an UPDATE.
    fn kept_candidates(&self, applied: &Applied) -> Vec<KeptRow> {
        if !self
            .browse
            .as_ref()
            .is_some_and(|b| b.target().stable_key())
        {
            return Vec::new();
        }
        self.save_rows
            .iter()
            .enumerate()
            .filter_map(|(change, at)| match at {
                GridRow::Loaded(row) => match self.edits.row(*row)? {
                    staged::RowEdit::Update { key, cells } => {
                        let changed: Vec<_> = cells.values().cloned().collect();
                        // The re-run loads the row under the key the database wrote, which
                        // differs from the staged text for "010", NULL or DEFAULT.
                        let committed = applied
                            .written_key(change)
                            .cloned()
                            .unwrap_or_else(|| key.clone());
                        Some(KeptRow {
                            key: committed,
                            label: staged::kept_label(key, &changed),
                        })
                    }
                    staged::RowEdit::Delete { .. } => None,
                },
                GridRow::Inserted(_) => None,
            })
            .collect()
    }

    /// Ends the run's candidate check: a complete first result keeps those no loaded row has.
    fn check_kept(&mut self, complete: bool) {
        let candidates = std::mem::take(&mut self.kept_check);
        if !complete || candidates.is_empty() {
            return;
        }
        // One pass over the loaded rows, striking each candidate key found, until none is left.
        let mut missing: HashSet<RowKey> = candidates.iter().map(|c| c.key.clone()).collect();
        let rows = self.results.first().map_or(0, |r| r.store.rows());
        for row in 0..rows {
            if missing.is_empty() {
                break;
            }
            if let Some(key) = self.row_key(row) {
                missing.remove(&key);
            }
        }
        self.kept = candidates
            .into_iter()
            .filter(|c| missing.contains(&c.key))
            .collect();
    }

    fn start_apply(&mut self, cx: &mut Context<Self>) {
        let Some(state) = &self.browse else {
            self.save = SaveState::Idle;
            return;
        };
        let (changes, rows) = self.edits.changes();
        let batch = match EditBatch::new(state.target().clone(), changes) {
            Ok(batch) => batch,
            Err(e) => {
                self.banner = Some(format!("Nothing was committed: {e}"));
                self.save = SaveState::Idle;
                return;
            }
        };
        let title = staged::prompt_title(state.target().relation());
        let sql = staged::preview(&render_sql(&batch));
        self.save_rows = rows;
        self.save = SaveState::Applying;
        let spec = self.spec.clone();
        // A fresh session, never the tab's: it must not join a user's BEGIN or a parked stream.
        let work = tokio_bridge::spawn(cx, async move {
            let found = runner::resolve(&spec).await.map_err(Err)?;
            let mut options = dbento_core::SessionOptions::default();
            options.key_passphrase = found.key_passphrase;
            dbento_core::apply_with(&spec.workspace, &found.conn, found.secret, options, batch)
                .await
                .map_err(Ok)
        });
        self.save_task = Some(cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |m, cx| m.applied(result, title, sql, cx));
        }));
    }

    fn applied(&mut self, result: ApplyResult, title: String, sql: String, cx: &mut Context<Self>) {
        self.save = SaveState::Idle;
        match result {
            // Nobody is left to answer a closed tab's prompt, so its transaction ends here.
            Ok(Ok(applied)) if self.closed => {
                tokio_bridge::spawn(cx, applied.rollback()).detach();
            }
            Ok(Ok(applied)) => {
                self.prompts += 1;
                self.prompt = Some(ApplyPrompt {
                    id: self.prompts,
                    title,
                    detail: staged::prompt_detail(applied.affected()),
                    sql,
                });
                self.applied = Some(applied);
                self.save = SaveState::Prompt;
            }
            Ok(Err(Ok(ApplyError::Rejected { outcomes }))) => {
                self.edits.set_outcomes(&self.save_rows, &outcomes);
                self.banner = Some(staged::rejection_text(&outcomes));
                self.conflicts = self
                    .save_rows
                    .iter()
                    .zip(&outcomes)
                    .filter(|(at, o)| {
                        matches!(at, GridRow::Loaded(_)) && matches!(o, ChangeOutcome::Conflict)
                    })
                    .map(|(at, _)| *at)
                    .collect();
            }
            Ok(Err(Ok(e))) => self.banner = Some(format!("Nothing was committed: {e}")),
            Ok(Err(Err(message))) => {
                self.banner = Some(format!("Nothing was committed: {message}"))
            }
            Err(_) => self.banner = Some("Nothing was committed: the save stopped".into()),
        }
        cx.notify();
    }

    fn finish(&mut self, commit: bool, cx: &mut Context<Self>) {
        let Some(applied) = self.applied.take() else {
            return;
        };
        self.prompt = None;
        self.save = SaveState::Finishing;
        // A run the user starts meanwhile wins: the commit's own re-run is skipped then.
        self.finish_run = self.run_id;
        // Taken now, while the saved rows and the browsed table still describe this save.
        let changes = self.save_rows.len();
        let relation = self.browse.as_ref().map(|b| b.target().relation().clone());
        let kept = self.kept_candidates(&applied);
        let file = runner::sqlite_file(&self.spec).filter(|_| commit);
        let work = tokio_bridge::spawn(cx, async move {
            if commit {
                // Reads parked on the file hold its shared lock, which would fail the COMMIT.
                if let Some(file) = file {
                    parks::release(&file, RELEASE_WAIT).await;
                }
                applied.commit().await
            } else {
                applied.rollback().await
            }
        });
        self.save_task = Some(cx.spawn(async move |this, cx| {
            let result = work.await;
            let _ = this.update(cx, |m, cx| {
                m.save = SaveState::Idle;
                match result {
                    Ok(Ok(())) if commit => {
                        m.edits.clear();
                        m.edit_history.clear();
                        m.banner = None;
                        if let Some(relation) = relation {
                            cx.emit(BrowseEvent::Committed { changes, relation });
                        }
                        if m.run_id == m.finish_run
                            && let Some(state) = m.browse.clone()
                        {
                            m.kept_next = kept;
                            m.set_browse(state, cx);
                        }
                    }
                    Ok(Err(e)) if commit => m.banner = Some(staged::commit_error_text(&e)),
                    Err(_) if commit => {
                        m.banner = Some(staged::commit_error_text(&ApplyError::CommitUnknown(
                            ExecuteError::SessionClosed,
                        )))
                    }
                    // A failed ROLLBACK closed its session, and the server rolls back on close.
                    _ => {}
                }
                cx.notify();
            });
        }));
    }

    /// The row's identity read from its loaded cells; None outside an editable result.
    fn row_key(&self, row: u64) -> Option<RowKey> {
        let store = &self.results.first()?.store;
        let cells = store
            .columns()
            .iter()
            .enumerate()
            .map(|(c, info)| Some((info.name(), store.cell(row, c)?)))
            .collect::<Option<Vec<_>>>()?;
        staged::row_key(self.browse.as_ref()?.target().identity(), &cells)
    }

    /// Row operations need an editable result and no save in flight.
    fn can_edit_rows(&self) -> bool {
        self.save == SaveState::Idle && self.editable()
    }

    /// The `settable` checks, keeping what staging needs: the column name, and for a
    /// loaded row its key and loaded value. Unlike the in-cell editor, any value kind counts.
    fn settable_cell(&self, at: GridRow, col: usize) -> Option<SettableCell> {
        if !self.can_edit_rows() {
            return None;
        }
        let store = &self.results[0].store;
        let column = store.columns().get(col)?.name();
        let target = self.browse.as_ref()?.target();
        if !target.column(column).is_some_and(|c| c.editable()) {
            return None;
        }
        let (loaded, shown) = match at {
            GridRow::Loaded(row) => {
                if row >= store.rows() || self.edits.is_deleted(row) {
                    return None;
                }
                let original = staged::edit_value(store.cell(row, col)?)?;
                let shown = self.edits.cell(row, col).unwrap_or(&original).clone();
                (Some((self.row_key(row)?, original)), shown)
            }
            GridRow::Inserted(ix) => {
                if ix >= self.edits.inserted() {
                    return None;
                }
                let shown = self.edits.new_cell(ix, col).cloned();
                (None, shown.unwrap_or(EditValue::Default))
            }
        };
        Some(SettableCell {
            column: column.to_owned(),
            loaded,
            shown,
        })
    }

    /// Stages `value` on a cell `settable_cell` returned for `at`.
    fn stage_settable(
        &mut self,
        at: GridRow,
        col: usize,
        cell: SettableCell,
        value: EditValue,
        cx: &mut Context<Self>,
    ) -> bool {
        let staged = self.track(|edits| match (at, cell.loaded) {
            (GridRow::Loaded(row), Some((key, original))) => {
                edits.stage(row, key, col, &cell.column, &original, value)
            }
            (GridRow::Inserted(ix), _) => edits.stage_new(ix, col, &cell.column, value),
            _ => false,
        });
        cx.notify();
        staged
    }

    /// Turns a landed target and its keys into a `BrowseState` carrying the pending filter (if
    /// any) and browses it, or emits `BrowseEvent::Unavailable(message)`. The pump calls it
    /// after applying events.
    pub fn flush_browse(&mut self, cx: &mut Context<Self>) {
        let Some(landed) = self.target_landed.take() else {
            return;
        };
        let browse = Browse::new(self.target_filter.take(), self.target_sort.take());
        let state = landed.and_then(|(t, keys)| {
            BrowseState::new(t, browse)
                .map(|s| s.with_foreign_keys(keys))
                .map_err(|e| e.to_string())
        });
        match state {
            Ok(state) => self.set_browse(state, cx),
            Err(message) => {
                self.announce_fresh = false;
                cx.emit(BrowseEvent::Unavailable(message));
            }
        }
    }

    /// Marks the browse stale after a column change. With nothing staged the pump loads it
    /// fresh; staged edits stay, visible but unsaveable, until the user runs it again.
    fn columns_changed(&mut self) {
        self.stale = true;
        self.notice = (!self.edits.is_empty()).then(|| COLUMNS_BLOCKED.into());
    }

    // Asks for the target held behind a run once that run has ended.
    fn flush_target(&mut self) {
        if !self.target_queued || self.running {
            return;
        }
        self.target_queued = false;
        if let (Some(rel), Some(r)) = (&self.target_pending, &self.runner) {
            r.send(Command::Target { rel: rel.clone() });
        }
    }

    /// Starts the fresh load a stale browse with nothing staged is due, once nothing runs.
    pub fn flush_stale(&mut self, cx: &mut Context<Self>) {
        if self.stale
            && self.edits.is_empty()
            && !self.running
            && self.save == SaveState::Idle
            && self.target_pending.is_none()
        {
            self.fresh_load(cx);
        }
    }

    // Describes the browsed table again with the same filter and sort; its browse run says why.
    fn fresh_load(&mut self, cx: &mut Context<Self>) {
        let (Some(state), Some(browse)) = (&self.browse, self.browse_settings()) else {
            return;
        };
        let rel = state.target().relation().clone();
        self.stale = false;
        self.announce_fresh = true;
        self.load_target_browse(rel, browse, cx);
    }

    /// Records this tab's ended statements into `log`.
    pub fn set_history(&mut self, log: Entity<HistoryLog>) {
        self.history = Some(log);
    }

    /// Hands every statement ended since the last call to the log; a no-op without one.
    pub fn flush_history(&mut self, cx: &mut Context<Self>) {
        if let Some(log) = &self.history
            && !self.unrecorded.is_empty()
        {
            let entries = std::mem::take(&mut self.unrecorded);
            log.update(cx, |log, cx| log.append(entries, cx));
        }
    }

    /// The live runner's event stream, for tests.
    pub fn events(&self) -> Option<Receiver<Event>> {
        self.runner.as_ref().map(Runner::events)
    }

    /// What the runner connects with.
    pub fn spec(&self) -> &RunnerSpec {
        &self.spec
    }

    /// Which connection the tab is on: it changes on every switch or reconnect, since a
    /// connection's name stays while an edit of dbento.toml points it at another database.
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// The current run's id; 0 before the first run.
    pub fn run_id(&self) -> u64 {
        self.run_id
    }

    /// Whether a run is in progress and not parked on its last result or at its row cap. A
    /// statement fetching past its cap is running, so Stop stays live.
    pub fn running(&self) -> bool {
        self.running && self.paused.is_none() && self.parked_at_cap().is_none()
    }

    /// Whether a parked result has more rows to pull and no pull is in flight.
    pub fn has_more(&self) -> bool {
        self.paused.is_some() && !self.pulling
    }

    /// Whether the model was closed; it starts nothing new.
    pub fn closed(&self) -> bool {
        self.closed
    }

    /// The parked statement's script index.
    pub fn paused(&self) -> Option<usize> {
        self.paused
    }

    /// The statement parked at its cap and not fetching.
    pub fn parked_at_cap(&self) -> Option<usize> {
        (self.cap.as_ref())
            .filter(|c| !c.ended && !c.fetching)
            .map(|c| c.stmt)
    }

    /// `stmt`'s cap state; None unless it has parked at its cap in the current run.
    pub fn cap_view(&self, stmt: usize) -> Option<CapView> {
        let cap = self.cap.as_ref().filter(|c| c.stmt == stmt)?;
        Some(if cap.ended {
            CapView::Ended {
                expired: cap.expired,
            }
        } else if cap.fetching {
            CapView::Fetching
        } else {
            CapView::Parked {
                rows: cap.outcome.rows,
            }
        })
    }

    /// Continues the parked statement by the run's own row limit; a no-op unless parked.
    pub fn fetch_next(&mut self) {
        if let Some(more) = self.limits.max_rows {
            self.fetch(Some(more));
        }
    }

    /// Continues the parked statement to its end or the memory budget; a no-op unless parked.
    pub fn fetch_all(&mut self) {
        self.fetch(None);
    }

    /// How result 0 would continue; None unless this tab browses a stable-key target, result 0
    /// ended `Truncated` by a park expiry, the run had a row cap, nothing runs or saves, and
    /// (for Keyset) the last loaded row's key has no NULL.
    pub fn continuation(&self) -> Option<Continuation> {
        let state = self.browse.as_ref()?;
        let ready = !self.closed
            && !self.running
            && self.save == SaveState::Idle
            && self.limits.max_rows.is_some()
            && !self.stale
            && state.target().stable_key()
            && self.browse_result()
            && matches!(self.cap_view(0), Some(CapView::Ended { expired: Some(_) }));
        let store = &self.results.first()?.store;
        if !ready || !matches!(store.end(), Some(StatementEnd::Truncated(_))) {
            return None;
        }
        if state.browse().sort.is_some() {
            return Some(Continuation::Skip(store.rows()));
        }
        let key = self.row_key(store.rows().checked_sub(1)?)?;
        let keyed = key
            .values()
            .iter()
            .all(|(_, v)| !matches!(v, EditValue::Null));
        keyed.then_some(Continuation::Keyset)
    }

    /// Starts that continuation as a new run whose rows append to result 0, up to the run's own
    /// row limit more; result 0's SQL, source, staged edits and undo steps stay. A Skip emits
    /// `BrowseEvent::Skipped { rows: n }`. False when `continuation()` is None.
    pub fn continue_browse(&mut self, cx: &mut Context<Self>) -> bool {
        // A continuation implies the browse, the row limit and the expired cap unpacked here.
        let (Some(how), Some(state), Some(limit), Some(cap), Some(expired)) = (
            self.continuation(),
            &self.browse,
            self.limits.max_rows,
            &self.cap,
            self.cap.as_ref().and_then(|c| c.expired),
        ) else {
            return false;
        };
        // Unsorted resumes after the last key; sorted re-runs as is and drops what it repeats.
        let (sql, max_rows, skip) = match how {
            Continuation::Keyset => {
                let last = self.results[0].store.rows() - 1;
                let Some(key) = self.row_key(last) else {
                    return false;
                };
                let after = state.browse().clone().with_after(Keyset::new(key));
                match browse_sql(state.target(), &after) {
                    Ok(sql) => (sql, limit, 0),
                    Err(_) => return false,
                }
            }
            Continuation::Skip(n) => (state.sql().to_owned(), n.saturating_add(limit), n),
        };
        self.append = Some(Append {
            skip,
            dropped: 0,
            outcome: cap.outcome.clone(),
            expired,
            drifted: false,
            base: self.results[0].elapsed.unwrap_or_default(),
        });
        self.ticking = Some((0, Instant::now()));
        if self.runner.is_none() {
            self.start_runner(cx);
        }
        self.run_id += 1;
        self.running = true;
        self.cap = None;
        self.paused = None;
        self.pulling = false;
        self.stopping = false;
        self.withdrawn = false;
        self.notice = None;
        self.results[0].store.reopen();
        if let Some(r) = &self.runner {
            r.send(Command::Run {
                run_id: self.run_id,
                sql,
                limits: AppLimits {
                    max_rows: Some(max_rows),
                    ..self.limits
                },
            });
        }
        if let Continuation::Skip(rows) = how {
            cx.emit(BrowseEvent::Skipped { rows });
        }
        cx.notify();
        true
    }

    /// Not a browse tab, the last result ended `Truncated` by a park expiry, the run text is
    /// one statement (`statement_count`, so a SurrealDB unit of several never re-runs), and the
    /// row limit has a next step.
    pub fn can_run_again(&self) -> bool {
        // Only read-only statements park, so an expired result needs no write check here.
        let Some(last) = self.results.last() else {
            return false;
        };
        let one = self
            .run_text
            .as_ref()
            .is_some_and(|(engine, text)| statement_count(*engine, text) == 1);
        self.browse.is_none()
            && !self.closed
            && !self.running
            && one
            && matches!(last.store.end(), Some(StatementEnd::Truncated(_)))
            && matches!(
                self.cap_view(last.stmt),
                Some(CapView::Ended { expired: Some(_) })
            )
            && raise_target(self.max_rows).is_some()
    }

    /// Sets the limit to the next `ROW_LIMITS` step and re-runs the run text with `run_at`
    /// from the editor offset it started at. False unless `can_run_again`.
    pub fn run_again(&mut self, cx: &mut Context<Self>) -> bool {
        let ready = self.can_run_again() && !self.commit_holds_edits();
        let Some((_, text)) = self.run_text.clone().filter(|_| ready) else {
            return false;
        };
        self.max_rows = raise_target(self.max_rows);
        let start = self.run_start;
        self.run_at(text, start, cx);
        true
    }

    // A continuation's end counts result 0's rows. One that fails or is stopped ends Truncated
    // as before, with the earlier expiry, so it can be tried again; a failure says why.
    fn appended_end(&mut self, stmt: usize, end: StatementEnd) -> StatementEnd {
        let rows = self.entry(stmt).store.rows();
        // A drifted continuation landed nothing however it ended; result 0 stands as before it.
        if let Some(append) = self.append.as_ref().filter(|a| a.drifted) {
            let mut o = append.outcome.clone();
            o.rows = rows;
            self.cap = Some(Self::restored_cap(append));
            self.columns_changed();
            return StatementEnd::Truncated(o);
        }
        match end {
            StatementEnd::Complete(mut o) => {
                o.rows = rows;
                StatementEnd::Complete(o)
            }
            StatementEnd::Truncated(mut o) => {
                o.rows = rows;
                StatementEnd::Truncated(o)
            }
            other @ (StatementEnd::Skipped
            | StatementEnd::Failed(_)
            | StatementEnd::Stopped { .. }
            | StatementEnd::Planned) => {
                if let StatementEnd::Failed(e) = &other {
                    self.notice = Some(format!("Could not fetch more: {e}"));
                }
                let Some(append) = &self.append else {
                    return other;
                };
                let mut o = append.outcome.clone();
                o.rows = rows;
                self.cap = Some(Self::restored_cap(append));
                StatementEnd::Truncated(o)
            }
        }
    }

    /// Result 0's cap as it stood before the continuation: ended by its expiry.
    fn restored_cap(append: &Append) -> Cap {
        Cap {
            stmt: 0,
            outcome: append.outcome.clone(),
            fetching: false,
            ended: true,
            expired: Some(append.expired),
        }
    }

    // Only a parked statement is sent a Fetch: one sent while fetching could reach the runner
    // just before it re-parks and extend the stream a second time, and one after a Stop is moot.
    fn fetch(&mut self, more: Option<u64>) {
        if self.parked_at_cap().is_none() || self.stopping {
            return;
        }
        if let (Some(r), Some(cap)) = (&self.runner, &mut self.cap) {
            r.send(Command::Fetch {
                run_id: self.run_id,
                more,
            });
            cap.fetching = true;
            self.ticking = Some((cap.stmt, Instant::now()));
        }
    }

    /// `stmt`'s time as of `now`: its last reported time, plus the time since a Fetch or
    /// continuation of it began while that is still loading. None until it first reports.
    pub fn elapsed_at(&self, stmt: usize, now: Instant) -> Option<Duration> {
        let reported = self.results.iter().find(|r| r.stmt == stmt)?.elapsed?;
        let since = (self.ticking)
            .filter(|&(s, _)| s == stmt && self.running)
            .map_or(Duration::ZERO, |(_, at)| now.saturating_duration_since(at));
        Some(reported + since)
    }

    /// Whether a Fetch or continuation is loading, so a shown time is still counting.
    pub fn ticking(&self) -> bool {
        self.ticking.is_some() && self.running
    }

    // A continuation's runner times only itself; result 0 shows the whole browse's time.
    fn browse_elapsed(&self, elapsed: Duration) -> Duration {
        self.append.as_ref().map_or(elapsed, |a| a.base + elapsed)
    }

    /// Whether the runner is discarding a run's unread rows.
    pub fn settling(&self) -> bool {
        self.settling
    }

    /// The last settle failure or read-write disconnect; a new run clears it.
    pub fn notice(&self) -> Option<&str> {
        self.notice.as_deref()
    }

    /// The open session's database, as the runner labelled it, while it differs from the
    /// configured one; None once the session ends.
    pub fn current_database(&self) -> Option<&str> {
        self.current_database.as_deref()
    }

    /// Sets the row cap for the next run.
    pub fn set_max_rows(&mut self, max_rows: Option<u64>) {
        self.max_rows = max_rows;
    }

    /// Sets the memory budget for the next run's results.
    pub fn set_max_result_bytes(&mut self, bytes: u64) {
        self.max_result_bytes = bytes;
    }

    /// The row cap the next run uses.
    pub fn max_rows(&self) -> Option<u64> {
        self.max_rows
    }

    /// The current run's limits.
    pub fn run_limits(&self) -> AppLimits {
        self.limits
    }

    /// Whether the session is read-only, once connected.
    pub fn read_only(&self) -> Option<bool> {
        self.read_only
    }

    /// How the session refuses writes, once connected; None while not connected or not read-only.
    pub fn read_only_level(&self) -> Option<ReadOnlyEnforcement> {
        self.read_only_level
    }

    /// The current run's statements, in script order.
    pub fn results(&self) -> &[StatementResult] {
        &self.results
    }

    /// Statement `stmt`'s view while its names equal the result's column names; None otherwise.
    pub fn column_view(&self, stmt: usize) -> Option<&ColumnView> {
        let columns = self.results.get(stmt)?.store.columns();
        self.column_views.get(&stmt).filter(|v| {
            v.names().len() == columns.len()
                && v.names().iter().zip(columns).all(|(n, c)| n == c.name())
        })
    }

    /// Result `stmt`'s view to change, made afresh when none fits its columns; None without
    /// that result.
    fn column_view_mut(&mut self, stmt: usize) -> Option<&mut ColumnView> {
        if self.column_view(stmt).is_none() {
            let names = self.results.get(stmt)?.store.columns();
            let names = names.iter().map(|c| c.name().to_owned()).collect();
            self.column_views.insert(stmt, ColumnView::new(names));
        }
        self.column_views.get_mut(&stmt)
    }

    /// Hides data column `data` of result `stmt`; false when `ColumnView::hide` refuses.
    pub fn hide_column(&mut self, stmt: usize, data: usize, cx: &mut Context<Self>) -> bool {
        let hidden = self.column_view_mut(stmt).is_some_and(|v| v.hide(data));
        if hidden {
            cx.notify();
        }
        hidden
    }

    /// Shows every column of result `stmt`; false when none was hidden.
    pub fn show_all_columns(&mut self, stmt: usize, cx: &mut Context<Self>) -> bool {
        let shown = self.column_view_mut(stmt).is_some_and(ColumnView::show_all);
        if shown {
            cx.notify();
        }
        shown
    }

    /// Sets data column `data` to `fit_width_at` the grid font size over its name, type and
    /// `max_chars`.
    pub fn fit_column(&mut self, stmt: usize, data: usize, cx: &mut Context<Self>) -> bool {
        let size = settings::grid_font(cx).1;
        let Some(width) = self.results.get(stmt).and_then(|r| {
            let info = r.store.columns().get(data)?;
            Some(fit_width_at(
                info.name(),
                info.type_name(),
                r.store.max_chars(data),
                size,
            ))
        }) else {
            return false;
        };
        if let Some(view) = self.column_view_mut(stmt) {
            view.set_width(data, width);
        }
        cx.notify();
        true
    }

    /// Records user-resized widths, given as (data column, width).
    pub fn set_column_widths(
        &mut self,
        stmt: usize,
        widths: &[(usize, Pixels)],
        cx: &mut Context<Self>,
    ) {
        if widths.is_empty() {
            return;
        }
        if let Some(view) = self.column_view_mut(stmt) {
            for &(data, width) in widths {
                view.set_width(data, width);
            }
            cx.notify();
        }
    }

    /// The statement waiting on a confirmation, if any.
    pub fn confirm(&self) -> Option<&PendingConfirm> {
        self.confirm.as_ref()
    }

    /// The session as a link. A session error outranks an open session, so a stopped session
    /// reads Failed until a run reconnects; `error` is cleared on every connect. A session
    /// that ended after it opened reads Idle until the next connect.
    pub fn link(&self) -> LinkState {
        if self.closed {
            LinkState::Idle
        } else if let Some(e) = &self.error {
            LinkState::Failed(e.clone())
        } else if self.runner.is_none() {
            LinkState::Idle
        } else if self.connected {
            LinkState::Connected
        } else if self.read_only.is_some() {
            LinkState::Idle
        } else {
            LinkState::Connecting
        }
    }

    /// The last connection or session error.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// How many times a cancel handle was fired, for tests.
    pub fn cancels_fired(&self) -> u64 {
        self.cancels_fired
    }

    fn fire_cancel(&mut self) {
        if let Some(c) = &self.cancel {
            c.cancel();
            self.cancels_fired += 1;
        }
    }

    fn mark_started(&mut self, stmt: usize) {
        self.started.entry(stmt).or_insert_with(SystemTime::now);
    }

    fn history_entry(
        &self,
        stmt: usize,
        sql: &str,
        elapsed: Duration,
        outcome: HistoryOutcome,
    ) -> HistoryEntry {
        let at = self
            .started
            .get(&stmt)
            .copied()
            .unwrap_or_else(SystemTime::now);
        // The statement's terminator is not part of what it ran.
        let sql = sql.trim_end();
        let sql = sql.strip_suffix(';').unwrap_or(sql).trim_end();
        let ws = &self.spec.workspace;
        HistoryEntry::new(at, ws.dir(), &self.spec.connection, sql, elapsed, outcome)
    }

    fn record(&mut self, entry: HistoryEntry) {
        if self.history.is_some() {
            self.unrecorded.push(entry);
        }
    }

    // Records the current run's statements that have not ended as cut by `reason`; their
    // elapsed is the wall time since they started.
    fn record_unfinished(&mut self, reason: StopReason) {
        // Result 0 was recorded when it first ended; a continuation adds no entry.
        if self.append.is_some() {
            return;
        }
        let cut: Vec<HistoryEntry> = self
            .results
            .iter()
            .filter(|r| r.store.end().is_none() && r.plan.is_none())
            .map(|r| {
                let end = self.unfinished_end(r, reason);
                let elapsed = (self.started.get(&r.stmt))
                    .and_then(|t| t.elapsed().ok())
                    .unwrap_or_default();
                self.history_entry(r.stmt, &r.sql, elapsed, history_outcome(&end))
            })
            .collect();
        cut.into_iter().for_each(|e| self.record(e));
    }

    // Records and ends the current run's unfinished statements as cut by `reason`.
    fn finish_unfinished(&mut self, reason: StopReason) {
        self.record_unfinished(reason);
        let ends: Vec<(usize, StatementEnd)> = (self.results.iter().enumerate())
            .filter(|(_, r)| r.store.end().is_none())
            .map(|(ix, r)| (ix, self.unfinished_end(r, reason)))
            .collect();
        let cut = !ends.is_empty();
        for (ix, end) in ends {
            self.results[ix].store.finish(end);
        }
        if cut && let Some(append) = &self.append {
            self.cap = Some(Self::restored_cap(append));
        } else if let Some(cap) = &mut self.cap {
            cap.ended = true;
            cap.fetching = false;
        }
    }

    // A statement parked at its cap holds every row it was asked for, so a new run or a close
    // ends it Truncated with the rows it has; anything else is cut by `reason`.
    fn unfinished_end(&self, r: &StatementResult, reason: StopReason) -> StatementEnd {
        let rows = r.store.rows();
        if let Some(append) = &self.append {
            let mut t = append.outcome.clone();
            t.rows = rows;
            return StatementEnd::Truncated(t);
        }
        match &self.cap {
            Some(cap)
                if cap.stmt == r.stmt
                    && matches!(reason, StopReason::NewRun | StopReason::Closed) =>
            {
                let mut t = cap.outcome.clone();
                t.rows = rows;
                StatementEnd::Truncated(t)
            }
            _ => StatementEnd::Stopped { rows, reason },
        }
    }

    /// Only this tab's Cancel fires the session's cancel handle, so a cancelled statement is
    /// the user's stop: it keeps the rows it sent instead of reading as a failure.
    fn as_user_stop(&mut self, stmt: usize, end: StatementEnd) -> StatementEnd {
        if !matches!(end, StatementEnd::Failed(ExecuteError::Cancelled)) {
            return end;
        }
        StatementEnd::Stopped {
            rows: self.entry(stmt).store.rows(),
            reason: StopReason::User,
        }
    }

    /// The entry for `stmt`, created with no columns when the runner never reported it (a
    /// statement that failed while being prepared); its text is its slice of the run.
    fn entry(&mut self, stmt: usize) -> &mut StatementResult {
        let ix = match self.results.iter().position(|r| r.stmt == stmt) {
            Some(ix) => ix,
            None => {
                let engine = self.run_text.as_ref().map(|(engine, _)| *engine);
                let plan = self
                    .explain
                    .map(|(mode, unavailable)| PlanResult::new(mode, unavailable));
                let (sql, source) = self
                    .run_text
                    .as_ref()
                    .and_then(|(engine, text)| {
                        let range = result_ranges(*engine, text).get(stmt)?.clone();
                        let source = statement_source(*engine, text, self.run_start, stmt);
                        // An explain's text is one statement, named as the user gave it.
                        let sql = if plan.is_some() {
                            text.trim()
                        } else {
                            text[range].trim()
                        };
                        Some((sql.to_owned(), source))
                    })
                    .unwrap_or_default();
                self.results.push(StatementResult {
                    stmt,
                    sql,
                    engine,
                    // A retired runner's events never land, so a new result is this epoch's.
                    epoch: self.epoch,
                    store: ResultStore::new(Arc::new([])),
                    elapsed: None,
                    source,
                    plan,
                });
                self.results.len() - 1
            }
        };
        &mut self.results[ix]
    }

    /// Spawns a runner and the pump that feeds its events back. Storing the pump means a
    /// replaced runner's pump is dropped with it and never touches a later run.
    fn start_runner(&mut self, cx: &mut Context<Self>) {
        let runner = runner::spawn_tuned(&tokio_bridge::handle(cx), self.spec.clone(), self.tuning);
        let events = runner.events();
        self.pump = Some(cx.spawn(async move |this, cx| {
            // Apply everything queued in one update so a burst of batches renders once.
            while let Ok(first) = events.recv().await {
                let mut queued = vec![first];
                while let Ok(next) = events.try_recv() {
                    queued.push(next);
                }
                let alive = this.update(cx, |m, cx| {
                    queued.into_iter().for_each(|e| m.apply(e));
                    m.flush_history(cx);
                    m.flush_browse(cx);
                    m.flush_save(cx);
                    m.flush_stale(cx);
                    m.flush_target();
                    cx.notify();
                });
                if alive.is_err() {
                    return;
                }
            }
            // The runner ended without saying so: a run left waiting would never finish.
            let _ = this.update(cx, |m, cx| {
                if m.running || m.paused().is_some() || m.target_pending.is_some() {
                    m.apply(Event::Stopped {
                        message: "session stopped".into(),
                    });
                    m.flush_history(cx);
                    m.flush_browse(cx);
                    m.flush_save(cx);
                    cx.notify();
                }
            });
        }));
        // A retired runner's answer died with its pump, so the new one is asked again.
        if let Some(rel) = &self.target_pending {
            runner.send(Command::Target { rel: rel.clone() });
            self.target_queued = false;
        }
        self.runner = Some(runner);
    }
}

/// Same names and types in the same order: rows of one shape can stand in for the other's.
fn same_columns(a: &[ColumnInfo], b: &[ColumnInfo]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(x, y)| x.name() == y.name() && x.type_name() == y.type_name())
}

/// Whether a statement end rules its result out for editing.
fn failed_or_skipped(end: &StatementEnd) -> bool {
    matches!(end, StatementEnd::Failed(_) | StatementEnd::Skipped)
}

/// A cell `TabModel::settable` allows, with what staging it needs.
struct SettableCell {
    column: String,
    /// A loaded row's key and loaded value; None for an inserted row.
    loaded: Option<(RowKey, EditValue)>,
    shown: EditValue,
}

/// The in-cell editor's text for a shown value: "" for NULL or DEFAULT, single-line text as
/// is; None for bytes and multi-line text, which the editor cannot round-trip.
fn editor_text(shown: &EditValue) -> Option<String> {
    match shown {
        EditValue::Null | EditValue::Default => Some(String::new()),
        EditValue::Text(text) if !text.contains(['\n', '\r']) => Some(text.clone()),
        EditValue::Text(_) | EditValue::Bytes(_) => None,
        _ => None,
    }
}
