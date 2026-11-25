//! The app's update check: at most one automatic check a day over a state file, a manual
//! check that reports its result, the offer the banner shows, and the release prepared from
//! that offer and armed for install when the app quits.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use dbento_core::update::{
    self, Available, CheckError, InstallError, InstallKind, NotesError, Os, Product, Reason,
    Relaunch, ReleaseNotes, Staged,
};
use gpui_kit::{
    App, AppContext as _, Context, Entity, EventEmitter, Global, Subscription, Task, Window,
};
use serde::{Deserialize, Serialize};
use tokio::sync::watch;

use crate::shell::Shell;
use crate::{persist, settings_store, ui};

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

/// Fetches the release notes for the offered version: Ok(None) when the release lists none.
pub type Notes = Rc<dyn Fn(&mut App, String) -> Task<Result<Option<ReleaseNotes>, String>>>;

/// Where the offered release's notes stand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotesState {
    /// No offer, no notes hook, or a release that lists no notes.
    Unavailable,
    /// The notes are being fetched.
    Loading,
    /// The notes of the offered release.
    Loaded(ReleaseNotes),
    /// Fetching failed with this message; the offer stands.
    Failed(String),
}

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

/// Starts the processes an install hands over to; tests record the calls instead.
pub trait Launcher {
    /// Starts `path` once this process has exited.
    fn restart(&self, path: &Path) -> std::io::Result<()>;
    /// Starts the installer `program` with `args` now.
    fn start_installer(&self, program: &Path, args: &[OsString]) -> std::io::Result<()>;
}

/// The launcher that starts real processes.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemLauncher;

impl Launcher for SystemLauncher {
    // The waiting shell runs in its own process group with no stdio, so it outlives this
    // process and the terminal it may have been started from.
    fn restart(&self, path: &Path) -> std::io::Result<()> {
        let os = Os::current().ok_or_else(|| std::io::Error::other("unsupported system"))?;
        let mut command = relaunch_command(path, std::process::id(), os)
            .ok_or_else(|| std::io::Error::other("this system relaunches through its installer"))?;
        #[cfg(unix)]
        std::os::unix::process::CommandExt::process_group(&mut command, 0);
        command
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .map(drop)
    }

    fn start_installer(&self, program: &Path, args: &[OsString]) -> std::io::Result<()> {
        Command::new(program).args(args).spawn().map(drop)
    }
}

// The launcher of a build that never installs: nothing is started.
struct NoLauncher;

impl Launcher for NoLauncher {
    fn restart(&self, _: &Path) -> std::io::Result<()> {
        Ok(())
    }
    fn start_installer(&self, _: &Path, _: &[OsString]) -> std::io::Result<()> {
        Ok(())
    }
}

/// A shell command that waits for process `pid` to exit, then starts the install at `path`:
/// the AppImage itself on Linux, the bundle through `open` on macOS. `None` on Windows,
/// where the NSIS installer relaunches the app.
pub fn relaunch_command(path: &Path, pid: u32, os: Os) -> Option<Command> {
    let start = match os {
        Os::Linux => "exec \"$1\"",
        Os::Macos => "exec /usr/bin/open \"$1\"",
        Os::Windows => return None,
        _ => return None,
    };
    let script = format!("while kill -0 \"$0\" 2>/dev/null; do sleep 0.1; done; {start}");
    let mut command = Command::new("/bin/sh");
    command.arg("-c").arg(script).arg(pid.to_string()).arg(path);
    Some(command)
}

/// What the update model calls out to; tests pass fakes, the app passes [`UpdateHooks::real`].
pub struct UpdateHooks {
    checker: Option<Checker>,
    stager: Option<Stager>,
    notes: Option<Notes>,
    launcher: Rc<dyn Launcher>,
}

impl UpdateHooks {
    /// No checker: updates are disabled.
    pub fn none() -> Self {
        UpdateHooks {
            checker: None,
            stager: None,
            notes: None,
            launcher: Rc::new(NoLauncher),
        }
    }

    /// Checks with `checker`; without a stager the offer can only be downloaded by hand.
    pub fn checking(checker: Checker) -> Self {
        UpdateHooks {
            checker: Some(checker),
            stager: None,
            notes: None,
            launcher: Rc::new(NoLauncher),
        }
    }

    /// Prepares offered releases with `s`.
    pub fn with_stager(self, s: Stager) -> Self {
        UpdateHooks {
            stager: Some(s),
            ..self
        }
    }

    /// Fetches the offered release's notes with `n`.
    pub fn with_notes(self, n: Notes) -> Self {
        UpdateHooks {
            notes: Some(n),
            ..self
        }
    }

    /// Starts processes with `l`.
    pub fn with_launcher(self, l: Rc<dyn Launcher>) -> Self {
        UpdateHooks {
            launcher: l,
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
        // The notes hook reads the release the last check found, so it needs no check of its own.
        let found: Arc<Mutex<Option<Available>>> = Arc::default();
        let notes = real_notes(Arc::clone(&found));
        let checker: Checker = Rc::new(move |cx: &mut App| {
            let (cfg, found) = (cfg.clone(), Arc::clone(&found));
            let download_page = cfg.download_page().to_owned();
            // The check blocks on HTTP, so it runs on the tokio runtime and is awaited here.
            let job = crate::tokio_bridge::spawn(cx, async move { update::check(&cfg).await });
            cx.background_spawn(async move {
                match job.await {
                    Ok(result) => {
                        if let Ok(Some(a)) = &result
                            && let Ok(mut slot) = found.lock()
                        {
                            *slot = Some(a.clone());
                        }
                        check_result(result, &download_page)
                    }
                    Err(e) => Err(e.to_string()),
                }
            })
        });
        let hooks = UpdateHooks::checking(checker)
            .with_notes(notes)
            .with_launcher(Rc::new(SystemLauncher));
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

// Fetches the notes of the release the checker stored and refuses any other version, so the
// dialog never shows notes for a release the banner does not offer.
fn real_notes(found: Arc<Mutex<Option<Available>>>) -> Notes {
    Rc::new(move |cx: &mut App, version: String| {
        let stored = found.lock().ok().and_then(|slot| slot.clone());
        let Some(a) = stored.filter(|a| a.version() == version) else {
            return Task::ready(Err(format!("no checked release is version {version}")));
        };
        let job = crate::tokio_bridge::spawn(cx, async move { update::release_notes(&a).await });
        cx.background_spawn(async move {
            match job.await {
                Ok(Ok(notes)) => Ok(Some(notes)),
                Ok(Err(NotesError::NotPublished)) => Ok(None),
                Ok(Err(e)) => Err(e.to_string()),
                Err(e) => Err(e.to_string()),
            }
        })
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

/// The toast for a manual check's report: a failure warns, anything else informs.
pub fn report_notice(report: &Report) -> ui::Notice {
    let kind = match report {
        Report::Failed(_) => ui::NoticeKind::Warning,
        Report::UpToDate | Report::Available(_) | Report::Disabled => ui::NoticeKind::Info,
    };
    ui::Notice {
        text: report_text(report),
        kind,
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
    manual_pending: bool,
    stager: Option<Stager>,
    launcher: Rc<dyn Launcher>,
    phase: InstallPhase,
    prepared: Option<Prepared>,
    armed: bool,
    preparing: Option<Task<()>>,
    progress: Option<Task<()>>,
    notes: Option<Notes>,
    notes_state: NotesState,
    notes_job: Option<Task<()>>,
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
            manual_pending: false,
            stager: hooks.stager,
            launcher: hooks.launcher,
            phase: InstallPhase::Idle,
            prepared: None,
            armed: false,
            preparing: None,
            progress: None,
            notes: hooks.notes,
            notes_state: NotesState::Unavailable,
            notes_job: None,
        }
    }

    /// The offer the banner shows, if any.
    pub fn offer(&self) -> Option<&Offer> {
        self.offer.as_ref()
    }

    /// Where the offered release's notes stand.
    pub fn notes_state(&self) -> &NotesState {
        &self.notes_state
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

    /// Hides the offer for this run and cancels its download; a prepared release is kept.
    pub fn later(&mut self, cx: &mut Context<Self>) {
        self.offer = None;
        self.clear_notes();
        self.cancel_download();
        cx.notify();
    }

    /// Records the offered version as skipped and hides it; a release prepared for it is
    /// disarmed and discarded, so the quit never installs a skipped release.
    pub fn skip(&mut self, cx: &mut Context<Self>) {
        if let Some(offer) = self.offer.take() {
            self.state.skipped_version = Some(offer.version);
            self.save();
        }
        self.clear_notes();
        self.drop_release();
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

    // No offer, or a different version, disarms and discards the prepared release, so a
    // release is installed only while the banner shows its version.
    fn set_offer(&mut self, offer: Option<Offer>, cx: &mut Context<Self>) {
        match &offer {
            None => {
                self.clear_notes();
                self.drop_release();
            }
            Some(new) => {
                let fresh = self.offer.as_ref().is_none_or(|o| o.version != new.version);
                if fresh || matches!(self.notes_state, NotesState::Failed(_)) {
                    self.fetch_notes(new.version.clone(), cx);
                }
                if self
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
            }
        }
        self.offer = offer;
    }

    // A result lands only while the offer still holds the version it was fetched for, and
    // it never touches the offer itself.
    fn fetch_notes(&mut self, version: String, cx: &mut Context<Self>) {
        let Some(hook) = self.notes.as_ref().map(Rc::clone) else {
            self.clear_notes();
            return;
        };
        self.notes_state = NotesState::Loading;
        let job = hook(cx, version.clone());
        self.notes_job = Some(cx.spawn(async move |this, cx| {
            let result = job.await;
            let _ = this.update(cx, |u, cx| {
                if u.offer.as_ref().is_none_or(|o| o.version != version) {
                    return;
                }
                u.notes_job = None;
                u.notes_state = match result {
                    Ok(Some(notes)) => NotesState::Loaded(notes),
                    Ok(None) => NotesState::Unavailable,
                    Err(message) => NotesState::Failed(message),
                };
                cx.notify();
            });
        }));
    }

    // Dropping the job cancels a fetch for an offer that is gone.
    fn clear_notes(&mut self) {
        self.notes_job = None;
        self.notes_state = NotesState::Unavailable;
    }

    // Dropping the tasks cancels the download, so it can neither report back nor arm.
    fn cancel_download(&mut self) {
        self.preparing = None;
        self.progress = None;
        if matches!(self.phase, InstallPhase::Downloading { .. }) {
            self.phase = InstallPhase::Idle;
        }
    }

    // The release the user turned down: no download, nothing prepared, nothing armed.
    fn drop_release(&mut self) {
        self.cancel_download();
        self.armed = false;
        if let Some(dropped) = self.prepared.take() {
            dropped.discard();
        }
        self.phase = InstallPhase::Idle;
    }

    // The time is recorded before the check runs, so a failing server is tried once a day.
    // A check already in flight is not repeated; a manual one asked for meanwhile makes it
    // finish as manual, so the user still gets a report.
    fn start(&mut self, manual: bool, now: u64, cx: &mut Context<Self>) {
        let Some(checker) = self.checker.as_ref().map(Rc::clone) else {
            return;
        };
        if self.in_flight.is_some() {
            self.manual_pending |= manual;
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
        let manual = manual || std::mem::take(&mut self.manual_pending);
        match (result, manual) {
            (Ok(CheckResult::UpToDate), true) => {
                self.set_offer(None, cx);
                self.finish_report(Report::UpToDate, cx);
            }
            (Ok(CheckResult::Available(offer)), true) => {
                let report = Report::Available(offer.version.clone());
                self.set_offer(Some(offer), cx);
                self.finish_report(report, cx);
            }
            (Err(e), true) => self.finish_report(Report::Failed(e), cx),
            (Ok(CheckResult::UpToDate), false) => self.set_offer(None, cx),
            (Ok(CheckResult::Available(offer)), false) => {
                if self.state.skipped_version.as_deref() != Some(offer.version.as_str()) {
                    self.set_offer(Some(offer), cx);
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
    _quit: Subscription,
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
    // GPUI waits at most 200 ms for a quit hook's future, so the install runs in the body.
    let quit = cx.on_app_quit(move |cx| {
        quit_install(cx);
        async {}
    });
    cx.set_global(Installed {
        updates: updates.clone(),
        _schedule: schedule,
        _quit: quit,
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

/// Installs the armed release and starts what it hands over to; nothing when none is armed.
/// It runs while the app quits, so it touches no window and does its work synchronously.
pub fn quit_install(cx: &mut App) {
    let Some(updates) = global(cx) else {
        return;
    };
    let Some((prepared, launcher)) = updates.update(cx, |u, _| {
        u.take_armed().map(|p| (p, Rc::clone(&u.launcher)))
    }) else {
        return;
    };
    let launched = prepared
        .install()
        .map_err(|e| e.to_string())
        .and_then(|relaunch| {
            match relaunch {
                Relaunch::Restart(path) => launcher.restart(&path),
                Relaunch::Installer { program, args } => launcher.start_installer(&program, &args),
                Relaunch::Nothing => Ok(()),
                _ => Ok(()),
            }
            .map_err(|e| e.to_string())
        });
    if let Err(e) = launched {
        log::error!("update install: {e}");
    }
}

/// Prepares the offered release and, once it is ready, arms it and closes the window the way
/// the title bar does. A close the user keeps open leaves the release armed for a later quit.
pub fn download_and_restart(shell: &Entity<Shell>, window: &mut Window, cx: &mut App) {
    let Some(updates) = global(cx) else {
        return;
    };
    let (handle, shell, armer) = (window.window_handle(), shell.clone(), updates.clone());
    updates.update(cx, |u, cx| {
        u.prepare(
            move |ready, cx| {
                if !ready {
                    return;
                }
                armer.update(cx, |u, cx| u.arm(cx));
                let _ = handle.update(cx, |_, window, cx| {
                    shell.update(cx, |s, cx| s.title_close(window, cx));
                });
            },
            cx,
        );
    });
}
