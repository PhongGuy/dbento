//! The app's update check: at most one automatic check a day over a state file, a manual
//! check that reports its result, and the offer the banner shows.

use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use dbento_core::update::{self, Available, CheckError, Product};
use gpui_kit::{App, AppContext as _, Context, Entity, EventEmitter, Global, Task};
use serde::{Deserialize, Serialize};

use crate::{persist, settings_store};

/// When updates were last checked and which release the user chose to skip.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct UpdateState {
    /// The start of the last check, in unix seconds.
    pub last_check: Option<u64>,
    /// The release version the user skipped.
    pub skipped_version: Option<String>,
}

/// A newer release, as the banner presents it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Offer {
    /// The release version.
    pub version: String,
    /// The page the release downloads from.
    pub download_page: String,
}

/// What a checker found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CheckResult {
    /// This build is the newest release.
    UpToDate,
    /// A newer release exists.
    Available(Offer),
}

/// The outcome of a manual check, for the user.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Report {
    /// This build is the newest release.
    UpToDate,
    /// This version is available.
    Available(String),
    /// The check failed with this message.
    Failed(String),
    /// This build has no update check.
    Disabled,
}

/// Emitted by [`Updates`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdatesEvent {
    /// A manual check finished with this report.
    Reported(Report),
}

/// Runs one check; the error is a message for the user.
pub type Checker = Rc<dyn Fn(&mut App) -> Task<Result<CheckResult, String>>>;

/// What the update model calls out to; tests pass fakes, the app passes [`UpdateHooks::real`].
pub struct UpdateHooks {
    checker: Option<Checker>,
}

impl UpdateHooks {
    /// No checker: updates are disabled.
    pub fn none() -> Self {
        UpdateHooks { checker: None }
    }

    /// Checks with `checker`.
    pub fn checking(checker: Checker) -> Self {
        UpdateHooks {
            checker: Some(checker),
        }
    }

    /// The checker over `build_config(Product::App)`; none when this build is not configured.
    /// A configuration that is present but invalid gives a checker that reports why.
    pub fn real() -> Self {
        let cfg = match update::build_config(Product::App) {
            Ok(cfg) => cfg,
            Err(CheckError::Disabled) => return UpdateHooks::none(),
            Err(e) => {
                let message = e.to_string();
                return UpdateHooks::checking(Rc::new(move |_| Task::ready(Err(message.clone()))));
            }
        };
        UpdateHooks::checking(Rc::new(move |cx: &mut App| {
            let cfg = cfg.clone();
            let download_page = cfg.download_page().to_owned();
            // The check blocks on HTTP, so it runs on the tokio runtime and is awaited here.
            let job = crate::tokio_bridge::spawn(cx, async move { update::check(&cfg).await });
            cx.background_spawn(async move {
                match job.await {
                    Ok(result) => check_result(result, &download_page),
                    Err(e) => Err(e.to_string()),
                }
            })
        }))
    }
}

/// The least time between automatic checks.
pub const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// How often the schedule asks whether a check is due.
pub const POLL_INTERVAL: Duration = Duration::from_secs(60 * 60);

/// The sentence a manual check's report shows the user, in a toast or in Settings.
pub fn report_text(report: &Report) -> String {
    match report {
        Report::UpToDate => format!("Dbento {} is up to date.", env!("CARGO_PKG_VERSION")),
        Report::Available(version) => format!("Dbento {version} is available."),
        Report::Failed(message) => format!("Update check failed: {message}"),
        Report::Disabled => "Update checks are not configured for this build.".to_string(),
    }
}

/// What a core check means to the app: `Http(404)` (a release without `latest.json`) is up to date.
pub fn check_result(
    result: Result<Option<Available>, CheckError>,
    download_page: &str,
) -> Result<CheckResult, String> {
    match result {
        Ok(Some(a)) => Ok(CheckResult::Available(Offer {
            version: a.version().to_owned(),
            download_page: download_page.to_owned(),
        })),
        Ok(None) | Err(CheckError::Http(404)) => Ok(CheckResult::UpToDate),
        Err(e) => Err(e.to_string()),
    }
}

/// The update check's state, its checker and the offer currently shown.
pub struct Updates {
    path: Option<PathBuf>,
    state: UpdateState,
    checker: Option<Checker>,
    offer: Option<Offer>,
    report: Option<Report>,
    in_flight: Option<Task<()>>,
}

impl EventEmitter<UpdatesEvent> for Updates {}

impl Updates {
    /// Updates over the state file at `state_path` (None: in memory only).
    pub fn new(state_path: Option<PathBuf>, hooks: UpdateHooks) -> Self {
        let state = state_path
            .as_deref()
            .map(|p| persist::load_json::<UpdateState>(p).value)
            .unwrap_or_default();
        Updates {
            path: state_path,
            state,
            checker: hooks.checker,
            offer: None,
            report: None,
            in_flight: None,
        }
    }

    /// The offer the banner shows, if any.
    pub fn offer(&self) -> Option<&Offer> {
        self.offer.as_ref()
    }

    /// The outcome of the last manual check.
    pub fn last_report(&self) -> Option<&Report> {
        self.report.as_ref()
    }

    /// Automatic: only when `enabled`, a checker exists, and the last check is more than
    /// CHECK_INTERVAL before `now`. Hides a skipped release and only logs an error.
    pub fn check_if_due(&mut self, enabled: bool, now: SystemTime, cx: &mut Context<Self>) {
        if !enabled || self.checker.is_none() {
            return;
        }
        let now = unix_secs(now);
        // A last check in the future means the clock moved back, so it does not block one now.
        let due = self.state.last_check.is_none_or(|last| {
            now.checked_sub(last)
                .is_none_or(|since| since > CHECK_INTERVAL.as_secs())
        });
        if due {
            self.start(false, now, cx);
        }
    }

    /// Manual: ignores the setting and the skip, and reports its result.
    pub fn check_now(&mut self, cx: &mut Context<Self>) {
        if self.checker.is_none() {
            self.finish_report(Report::Disabled, cx);
            return;
        }
        self.start(true, unix_secs(SystemTime::now()), cx);
    }

    /// Hides the offer for this run.
    pub fn later(&mut self, cx: &mut Context<Self>) {
        self.offer = None;
        cx.notify();
    }

    /// Records the offered version as skipped and hides it.
    pub fn skip(&mut self, cx: &mut Context<Self>) {
        if let Some(offer) = self.offer.take() {
            self.state.skipped_version = Some(offer.version);
            self.save();
        }
        cx.notify();
    }

    // The time is recorded before the check runs, so a failing server is tried once a day.
    // A check already in flight makes this a no-op.
    fn start(&mut self, manual: bool, now: u64, cx: &mut Context<Self>) {
        let Some(checker) = self.checker.as_ref().map(Rc::clone) else {
            return;
        };
        if self.in_flight.is_some() {
            return;
        }
        self.state.last_check = Some(now);
        self.save();
        let job = checker(cx);
        self.in_flight = Some(cx.spawn(async move |this, cx| {
            let result = job.await;
            let _ = this.update(cx, |u, cx| u.finish(manual, result, cx));
        }));
    }

    fn finish(
        &mut self,
        manual: bool,
        result: Result<CheckResult, String>,
        cx: &mut Context<Self>,
    ) {
        self.in_flight = None;
        match (result, manual) {
            (Ok(CheckResult::UpToDate), true) => {
                self.offer = None;
                self.finish_report(Report::UpToDate, cx);
            }
            (Ok(CheckResult::Available(offer)), true) => {
                let report = Report::Available(offer.version.clone());
                self.offer = Some(offer);
                self.finish_report(report, cx);
            }
            (Err(e), true) => self.finish_report(Report::Failed(e), cx),
            (Ok(CheckResult::UpToDate), false) => self.offer = None,
            (Ok(CheckResult::Available(offer)), false) => {
                if self.state.skipped_version.as_deref() != Some(offer.version.as_str()) {
                    self.offer = Some(offer);
                }
            }
            (Err(e), false) => log::warn!("update check: {e}"),
        }
        cx.notify();
    }

    fn finish_report(&mut self, report: Report, cx: &mut Context<Self>) {
        self.report = Some(report.clone());
        cx.emit(UpdatesEvent::Reported(report));
        cx.notify();
    }

    // A state file that cannot be written only costs an extra check next run.
    fn save(&self) {
        if let Some(path) = &self.path
            && let Err(e) = persist::save_json(path, &self.state)
        {
            log::warn!("update state not saved to {}: {e}", path.display());
        }
    }
}

struct Installed {
    updates: Entity<Updates>,
    _schedule: Task<()>,
}
impl Global for Installed {}

/// Creates the global [`Updates`] and its schedule: every POLL_INTERVAL, starting at once, a
/// check runs if due. A tick before the settings store exists is skipped, so no check goes
/// out before the user's `check_for_updates` is read. A second call returns the installed one.
pub fn install(state_path: Option<PathBuf>, hooks: UpdateHooks, cx: &mut App) -> Entity<Updates> {
    if let Some(updates) = global(cx) {
        return updates;
    }
    let updates = cx.new(|_| Updates::new(state_path, hooks));
    let weak = updates.downgrade();
    let schedule = cx.spawn(async move |cx| {
        loop {
            let ticked = cx.update(|cx| {
                let enabled = settings_store::store(cx).map(|s| s.read(cx).get().check_for_updates);
                weak.update(cx, |u, cx| {
                    if let Some(enabled) = enabled {
                        u.check_if_due(enabled, SystemTime::now(), cx);
                    }
                })
            });
            if ticked.is_err() {
                return;
            }
            cx.background_executor().timer(POLL_INTERVAL).await;
        }
    });
    cx.set_global(Installed {
        updates: updates.clone(),
        _schedule: schedule,
    });
    updates
}

/// The installed [`Updates`], if any.
pub fn global(cx: &App) -> Option<Entity<Updates>> {
    cx.try_global::<Installed>().map(|i| i.updates.clone())
}

/// Runs the global's manual check; a no-op without one.
pub fn check_now(cx: &mut App) {
    if let Some(updates) = global(cx) {
        updates.update(cx, |u, cx| u.check_now(cx));
    }
}

fn unix_secs(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}
