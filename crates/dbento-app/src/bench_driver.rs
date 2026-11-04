//! The bench's frame-by-frame decisions and its report, kept free of GPUI so they test
//! without a window. The `bench` feature's window loop feeds [`Driver`] one frame at a time.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::runner::{StatementEnd, StopReason};

/// How the bench process ends: its exit code, and its last line on stderr.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BenchExit {
    /// The event loop ended before any report or error, as on a window close.
    #[default]
    Unreported,
    /// An error ended the bench and has already been printed.
    Failed,
    /// The report was written.
    Reported,
}

impl BenchExit {
    /// The process exit code: 0 only when the report was written.
    pub fn code(self) -> i32 {
        match self {
            BenchExit::Reported => 0,
            _ => 2,
        }
    }

    /// The reason still to print; only an unreported exit has not said why.
    pub fn message(self) -> Option<&'static str> {
        (self == BenchExit::Unreported).then_some("event loop ended before the report")
    }
}

/// One stderr line for a log record.
pub fn log_line(level: log::Level, target: &str, message: &str) -> String {
    format!("dbento-app bench: {level} {target}: {message}")
}

/// Writes `Warn` and worse to stderr through [`log_line`].
pub struct StderrLog;

impl log::Log for StderrLog {
    fn enabled(&self, metadata: &log::Metadata) -> bool {
        metadata.level() <= log::Level::Warn
    }

    fn log(&self, record: &log::Record) {
        if self.enabled(record.metadata()) {
            eprintln!(
                "{}",
                log_line(record.level(), record.target(), &record.args().to_string())
            );
        }
    }

    fn flush(&self) {}
}

/// The logger the bench installs, so gpui's own errors reach stderr.
pub static STDERR_LOG: StderrLog = StderrLog;

/// What the window loop does this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Ask for one more batch of the parked result.
    Pull,
    /// Nothing to do; rows are on their way.
    Wait,
    /// Write the report and quit.
    Finish,
}

/// Loads every row, stamping the frame that first showed rows, and finishes at the end.
#[derive(Debug, Default)]
pub struct Driver {
    done: bool,
    // The frame that first had rows; the frame after it is when they were on screen.
    paint_armed: bool,
    painted: Option<Instant>,
}

impl Driver {
    /// A driver at the start of a run.
    pub fn new() -> Driver {
        Driver::default()
    }

    /// One frame: `rows` loaded, `has_more` (parked with rows to pull), `ended` (statement
    /// or run done). A result that ends before its first rows were painted waits one frame.
    pub fn on_frame(&mut self, now: Instant, rows: usize, has_more: bool, ended: bool) -> Step {
        if self.done {
            return Step::Finish;
        }
        // Rows handed to the grid this frame are drawn by it, so the next frame stamps them.
        if self.paint_armed && self.painted.is_none() {
            self.painted = Some(now);
        }
        if rows > 0 {
            self.paint_armed = true;
        }
        if !ended {
            return if has_more { Step::Pull } else { Step::Wait };
        }
        if rows == 0 || self.painted.is_some() {
            self.done = true;
            return Step::Finish;
        }
        Step::Wait
    }

    /// Time from `run_start` to the frame that showed the first rows, once stamped.
    pub fn first_paint(&self, run_start: Instant) -> Option<Duration> {
        self.painted.map(|t| t.saturating_duration_since(run_start))
    }
}

/// Frames a fresh window draws before the bench presses Run. A fresh window's first two
/// frames are slow and would otherwise be charged to first paint, as a user never sees.
pub const WARM_FRAMES: usize = 60;

/// Counts a fresh window's frames and fires Run once they are drawn.
#[derive(Debug)]
pub struct Warmup {
    frames: usize,
    seen: usize,
    run: Option<Instant>,
}

impl Warmup {
    /// A warm-up that fires Run on the `frames`-th frame.
    pub fn new(frames: usize) -> Warmup {
        Warmup {
            frames,
            seen: 0,
            run: None,
        }
    }

    /// One frame; true on the frame that should press Run.
    pub fn on_frame(&mut self, now: Instant) -> bool {
        if self.run.is_some() {
            return false;
        }
        self.seen += 1;
        if self.seen < self.frames {
            return false;
        }
        self.run = Some(now);
        true
    }

    /// When Run fired, once it has.
    pub fn run_start(&self) -> Option<Instant> {
        self.run
    }
}

/// What the cancel loop does this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CancelStep {
    /// Nothing yet.
    Wait,
    /// Cancel the running statement now.
    Cancel,
    /// Done: the cancel's time to the end, or `None` when no cancel reached a running
    /// statement or the end never came.
    Finish(Option<Duration>),
}

/// Waits for the statement to execute, cancels it 500 ms later, and times the end.
#[derive(Debug)]
pub struct CancelDriver {
    run_start: Instant,
    started: Option<Instant>,
    cancelled: Option<Instant>,
}

impl CancelDriver {
    const AFTER: Duration = Duration::from_millis(500);
    const CAP: Duration = Duration::from_secs(10);

    /// A driver for a run started at `run_start`.
    pub fn new(run_start: Instant) -> CancelDriver {
        CancelDriver {
            run_start,
            started: None,
            cancelled: None,
        }
    }

    /// One frame: `executing` (the statement has begun), `ended` (it or the run has ended).
    /// A statement that ends before its cancel was sent finishes with no cancel time, and
    /// one with no end 10 s after the latest of run, start and cancel finishes the same way.
    pub fn on_frame(&mut self, now: Instant, executing: bool, ended: bool) -> CancelStep {
        if ended {
            return CancelStep::Finish(self.cancelled.map(|c| now.saturating_duration_since(c)));
        }
        if executing && self.started.is_none() {
            self.started = Some(now);
        }
        let since = |t: Instant| now.saturating_duration_since(t);
        let latest = self.cancelled.or(self.started).unwrap_or(self.run_start);
        if since(latest) > Self::CAP {
            return CancelStep::Finish(None);
        }
        match (self.started, self.cancelled) {
            (Some(started), None) if since(started) >= Self::AFTER => {
                self.cancelled = Some(now);
                CancelStep::Cancel
            }
            _ => CancelStep::Wait,
        }
    }
}

/// What a bench run measures.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BenchMode {
    /// Load a whole result, timing first paint and memory.
    Load,
    /// Start a statement and time its cancel.
    Cancel,
}

/// The job `xtask app-bench` hands the app, as JSON at the path in `DBENTO_APP_BENCH`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BenchConfig {
    /// The case name, copied into the report.
    pub case: String,
    /// What to measure.
    pub mode: BenchMode,
    /// The workspace connection to run on: "pg" or "lite".
    pub connection: String,
    /// The statement to run.
    pub sql: String,
    /// Where to write the [`BenchReport`].
    pub out: PathBuf,
}

/// One case's measurements. Its JSON field names are the contract with `xtask app-bench`.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct BenchReport {
    /// The case name from the config.
    pub case: String,
    /// Rows loaded.
    pub rows: u64,
    /// The result store's heap bytes at the end.
    pub heap_bytes: u64,
    /// VmHWM just before the run, in KiB.
    pub hwm_before_kib: u64,
    /// VmHWM once every row was loaded, in KiB.
    pub hwm_loaded_kib: u64,
    /// VmHWM at the end, in KiB.
    pub hwm_end_kib: u64,
    /// Run to the frame that showed the first rows.
    pub first_paint_ms: Option<f64>,
    /// Cancel to the statement's end; `None` when it did not end in time.
    pub cancel_ms: Option<f64>,
    /// How the statement ended, as [`end_label`] reads it.
    pub end: String,
}

/// "Complete", "Truncated", "Skipped", "Planned", "Failed: <error>", or "Stopped: " plus user,
/// new run, closed, lost or memory budget.
pub fn end_label(end: &StatementEnd) -> String {
    match end {
        StatementEnd::Complete(_) => "Complete".into(),
        StatementEnd::Truncated(_) => "Truncated".into(),
        StatementEnd::Skipped => "Skipped".into(),
        StatementEnd::Planned => "Planned".into(),
        StatementEnd::Failed(e) => format!("Failed: {e}"),
        StatementEnd::Stopped { reason, .. } => {
            let why = match reason {
                StopReason::User => "user",
                StopReason::NewRun => "new run",
                StopReason::Closed => "closed",
                StopReason::Lost => "lost",
                StopReason::MemoryBudget => "memory budget",
            };
            format!("Stopped: {why}")
        }
    }
}
