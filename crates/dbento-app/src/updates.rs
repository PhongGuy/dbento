//! The app's update check: at most one automatic check a day over a state file, a manual
//! check that reports its result, the offer the banner shows, and the release prepared from
//! that offer and armed for install when the app quits.

use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use dbento_core::update::{
    self, Available, CheckError, InstallError, InstallKind, Os, Product, Reason, Relaunch, Staged,
};
use gpui_kit::{App, AppContext as _, Context, Entity, EventEmitter, Global, Task};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

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

/// Reports a download's progress as `(received, total)` bytes.
pub type Progress = Box<dyn FnMut(u64, u64) + Send>;

/// Downloads and verifies the offered `version` for this install, reporting progress.
pub type Stager = Rc<dyn Fn(&mut App, String, Progress) -> Task<Result<Prepared, String>>>;

/// Where installing the offered release stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallPhase {
    /// Nothing is being prepared.
    Idle,
    /// The release is downloading; `total` is 0 until the first chunk arrives.
    Downloading {
        /// Bytes received so far.
        received: u64,
        /// The release's size in bytes.
        total: u64,
    },
    /// The release is verified and waits for the quit.
    Ready,
    /// Preparing failed with this message.
    Failed(String),
}

/// A verified release waiting to be installed when the app quits.
#[derive(Debug)]
pub struct Prepared {
    version: String,
    how: Install,
}

#[derive(Debug)]
enum Install {
    Staged(InstallKind, Staged),
    Only(Relaunch),
}

impl Prepared {
    /// The release `staged` for the install `kind`.
    pub fn new(version: String, kind: InstallKind, staged: Staged) -> Self {
        Prepared {
            version,
            how: Install::Staged(kind, staged),
        }
    }

    /// Installs nothing and relaunches as given; for tests.
    pub fn relaunch_only(version: impl Into<String>, relaunch: Relaunch) -> Self {
        Prepared {
            version: version.into(),
            how: Install::Only(relaunch),
        }
    }

    /// The release version.
    pub fn version(&self) -> &str {
        &self.version
    }

    /// `update::install` for a real release; `Ok(relaunch)` for `relaunch_only`.
    pub fn install(self) -> Result<Relaunch, InstallError> {
        match self.how {
            Install::Staged(kind, staged) => update::install(&kind, staged),
            Install::Only(relaunch) => Ok(relaunch),
        }
    }

    // The release holds `version` throughout, including the staged file inside it.
    fn holds(&self, version: &str) -> bool {
        self.version == version
            && match &self.how {
                Install::Staged(_, staged) => staged.version() == version,
                Install::Only(_) => true,
            }
    }

    // A release that will not be installed leaves no staged file behind; failure is harmless.
    fn discard(self) {
        if let Install::Staged(_, staged) = self.how {
            let _ = std::fs::remove_file(staged.path());
        }
    }
}

/// What the update model calls out to; tests pass fakes, the app passes [`UpdateHooks::real`].
pub struct UpdateHooks {
    checker: Option<Checker>,
    stager: Option<Stager>,
}

impl UpdateHooks {
    /// No checker: updates are disabled.
    pub fn none() -> Self {
        UpdateHooks {
            checker: None,
            stager: None,
        }
    }

    /// Checks with `checker`; without a stager the offer can only be downloaded by hand.
    pub fn checking(checker: Checker) -> Self {
        UpdateHooks {
            checker: Some(checker),
            stager: None,
        }
    }

    /// Prepares offered releases with `s`.
    pub fn with_stager(self, s: Stager) -> Self {
        UpdateHooks {
            stager: Some(s),
            ..self
        }
    }

    /// The checker over `build_config(Product::App)`; none when this build is not configured.
    /// A configuration that is present but invalid gives a checker that reports why. The
    /// stager is added only when the app owns its install (AppImage, `.app` or NSIS).
    pub fn real() -> Self {
        let cfg = match update::build_config(Product::App) {
            Ok(cfg) => cfg,
            Err(CheckError::Disabled) => return UpdateHooks::none(),
            Err(e) => {
                let message = e.to_string();
                return UpdateHooks::checking(Rc::new(move |_| Task::ready(Err(message.clone()))));
            }
        };
        let kind = local_kind();
        let owned = matches!(
            kind,
            InstallKind::AppImage(_) | InstallKind::MacApp(_) | InstallKind::WindowsNsis
        );
        let stager = owned.then(|| real_stager(cfg.clone(), kind));
        let checker: Checker = Rc::new(move |cx: &mut App| {
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
        });
        let hooks = UpdateHooks::checking(checker);
        match stager {
            Some(s) => hooks.with_stager(s),
            None => hooks,
        }
    }
}

// Sweeps stale staged files, checks again so only the release still offered for this install
// is fetched, then downloads and verifies it beside the target.
fn real_stager(cfg: update::UpdateConfig, kind: InstallKind) -> Stager {
    Rc::new(move |cx: &mut App, version: String, progress: Progress| {
        let (cfg, kind) = (cfg.clone(), kind.clone());
        let job = crate::tokio_bridge::spawn(cx, async move {
            let dir = update::staging_dir(&kind).map_err(|e| e.to_string())?;
            update::sweep_staged(&dir);
            let a = match update::check(&cfg).await {
                Ok(Some(a)) => a,
                Ok(None) => return Err("no newer release is available".to_string()),
                Err(e) => return Err(e.to_string()),
            };
            if a.version() != version {
                return Err(format!("the newest release is now {}", a.version()));
            }
            if !kind.accepts(a.format()) {
                return Err("the release is not packaged for this install".to_string());
            }
            let staged = update::download(&a, &dir, progress)
                .await
                .map_err(|e| e.to_string())?;
            Ok(Prepared::new(version, kind, staged))
        });
        cx.background_spawn(async move { job.await.map_err(|e| e.to_string())? })
    })
}

/// The install this process runs from, classified over its canonical executable and
/// `$APPIMAGE`; unrecognised when either the OS or the executable cannot be determined.
pub fn local_kind() -> InstallKind {
    let exe = std::env::current_exe().and_then(std::fs::canonicalize);
    let (Some(os), Ok(exe)) = (Os::current(), exe) else {
        return InstallKind::NotOwned(Reason::Unrecognised);
    };
    let appimage = std::env::var_os("APPIMAGE")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from);
    update::install_kind(
        Product::App,
        os,
        update::RELEASE_BUILD,
        &exe,
        appimage.as_deref(),
        false,
    )
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
    stager: Option<Stager>,
    phase: InstallPhase,
    prepared: Option<Prepared>,
    armed: bool,
    preparing: Option<Task<()>>,
    progress: Option<Task<()>>,
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
            stager: hooks.stager,
            phase: InstallPhase::Idle,
            prepared: None,
            armed: false,
            preparing: None,
            progress: None,
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

    /// A stager is installed and an offer is showing.
    pub fn can_install(&self) -> bool {
        self.stager.is_some() && self.offer.is_some()
    }

    /// Where installing the offered release stands.
    pub fn install_phase(&self) -> &InstallPhase {
        &self.phase
    }

    /// Prepares the offered release, or reuses the one prepared for it, then calls `on_done`
    /// (deferred, never inside this update) with whether it is ready.
    pub fn prepare(
        &mut self,
        on_done: impl FnOnce(bool, &mut App) + 'static,
        cx: &mut Context<Self>,
    ) {
        let (Some(stager), Some(offer)) = (self.stager.clone(), self.offer.as_ref()) else {
            cx.defer(move |cx| on_done(false, cx));
            return;
        };
        if matches!(self.phase, InstallPhase::Downloading { .. }) {
            cx.defer(move |cx| on_done(false, cx));
            return;
        }
        let version = offer.version.clone();
        if self.prepared.as_ref().is_some_and(|p| p.holds(&version)) {
            self.phase = InstallPhase::Ready;
            cx.notify();
            cx.defer(move |cx| on_done(true, cx));
            return;
        }
        // A release prepared for an older offer is stale; the new offer is fetched afresh.
        self.armed = false;
        if let Some(stale) = self.prepared.take() {
            stale.discard();
        }
        self.phase = InstallPhase::Downloading {
            received: 0,
            total: 0,
        };
        cx.notify();
        // The download reports from a blocking thread through a watch channel; a foreground
        // task mirrors it into the phase until the sender drops or the preparation ends.
        let (tx, mut rx) = watch::channel((0u64, 0u64));
        self.progress = Some(cx.spawn(async move |this, cx| {
            while rx.changed().await.is_ok() {
                let (received, total) = *rx.borrow_and_update();
                let shown = this.update(cx, |u, cx| {
                    if matches!(u.phase, InstallPhase::Downloading { .. }) {
                        u.phase = InstallPhase::Downloading { received, total };
                        cx.notify();
                    }
                });
                if shown.is_err() {
                    return;
                }
            }
        }));
        let job = stager(
            cx,
            version.clone(),
            Box::new(move |received, total| {
                tx.send_replace((received, total));
            }),
        );
        self.preparing = Some(cx.spawn(async move |this, cx| {
            let result = job.await;
            let ok = this
                .update(cx, |u, cx| u.finish_prepare(&version, result, cx))
                .unwrap_or(false);
            cx.update(|cx| on_done(ok, cx));
        }));
    }

    /// Marks the prepared release for install at the next quit; ignored without one.
    pub fn arm(&mut self, cx: &mut Context<Self>) {
        self.armed = self.prepared.is_some();
        cx.notify();
    }

    /// Clears the mark and keeps the prepared release.
    pub fn disarm(&mut self, cx: &mut Context<Self>) {
        self.armed = false;
        cx.notify();
    }

    /// A release is prepared and marked for install.
    pub fn armed(&self) -> bool {
        self.armed && self.prepared.is_some()
    }

    /// The prepared release when armed; clears both.
    pub fn take_armed(&mut self) -> Option<Prepared> {
        if !self.armed() {
            return None;
        }
        self.armed = false;
        self.phase = InstallPhase::Idle;
        self.prepared.take()
    }

    // Only a release for the version asked for, which is still the one offered, is kept;
    // anything else would install a release the banner no longer shows.
    fn finish_prepare(
        &mut self,
        requested: &str,
        result: Result<Prepared, String>,
        cx: &mut Context<Self>,
    ) -> bool {
        self.preparing = None;
        self.progress = None;
        let offered = self.offer.as_ref().map(|o| o.version.as_str());
        let ok = match result {
            Ok(prepared) if !prepared.holds(requested) || offered != Some(requested) => {
                prepared.discard();
                self.phase = InstallPhase::Failed(format!(
                    "the downloaded release is not version {}",
                    offered.unwrap_or(requested)
                ));
                false
            }
            Ok(prepared) => {
                self.prepared = Some(prepared);
                self.phase = InstallPhase::Ready;
                true
            }
            Err(message) => {
                self.phase = InstallPhase::Failed(message);
                false
            }
        };
        cx.notify();
        ok
    }

    // A different version disarms and discards the prepared release, so a release is
    // installed only after a fresh press for the version the banner shows.
    fn set_offer(&mut self, offer: Option<Offer>) {
        if let Some(new) = &offer
            && self
                .prepared
                .as_ref()
                .is_some_and(|p| !p.holds(&new.version))
        {
            self.armed = false;
            if let Some(stale) = self.prepared.take() {
                stale.discard();
            }
            self.phase = InstallPhase::Idle;
        }
        self.offer = offer;
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
                self.set_offer(None);
                self.finish_report(Report::UpToDate, cx);
            }
            (Ok(CheckResult::Available(offer)), true) => {
                let report = Report::Available(offer.version.clone());
                self.set_offer(Some(offer));
                self.finish_report(report, cx);
            }
            (Err(e), true) => self.finish_report(Report::Failed(e), cx),
            (Ok(CheckResult::UpToDate), false) => self.set_offer(None),
            (Ok(CheckResult::Available(offer)), false) => {
                if self.state.skipped_version.as_deref() != Some(offer.version.as_str()) {
                    self.set_offer(Some(offer));
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
