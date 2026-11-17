//! Preparing an offered release, its progress, and arming it for the next quit.
use crate::support;
use dbento_app::settings_store;
use dbento_app::theme::Appearance;
use dbento_app::updates::{
    self, CheckResult, Checker, InstallPhase, Offer, Prepared, Progress, Stager, UpdateHooks,
    Updates,
};
use dbento_core::update::Relaunch;
use gpui_kit::{App, AppContext as _, Entity, Task, TestAppContext};
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use tokio::sync::oneshot;

pub(crate) const IMAGE: &str = "/apps/Dbento.AppImage";

pub(crate) struct Rig {
    pub(crate) _dir: tempfile::TempDir,
    pub(crate) updates: Entity<Updates>,
    pub(crate) staged: Rc<Cell<usize>>,
    pub(crate) answer: Rc<RefCell<CheckResult>>,
}

pub(crate) fn available(version: &str) -> CheckResult {
    CheckResult::Available(Offer {
        version: version.into(),
        download_page: "https://example.com/download".into(),
    })
}

/// A stager that reports 5 of 10 bytes, waits for `gate` when one is left, then hands back a
/// release that installs nothing and relaunches as `then`. `fail` makes it fail instead.
pub(crate) fn stager(
    then: Relaunch,
    gate: Option<oneshot::Receiver<()>>,
    fail: bool,
) -> (Stager, Rc<Cell<usize>>) {
    let gate = Rc::new(RefCell::new(gate));
    let calls = Rc::new(Cell::new(0));
    let seen = Rc::clone(&calls);
    let s: Stager = Rc::new(
        move |cx: &mut App, version: String, mut progress: Progress| {
            seen.set(seen.get() + 1);
            progress(5, 10);
            let (wait, then) = (gate.borrow_mut().take(), then.clone());
            cx.background_spawn(async move {
                if let Some(rx) = wait {
                    let _ = rx.await;
                }
                if fail {
                    return Err("disk full".to_string());
                }
                Ok(Prepared::relaunch_only(version, then))
            })
        },
    );
    (s, calls)
}

/// The installed updates over a checker offering whatever `answer` holds (99.0.0 at first),
/// with `extra` applied to the hooks, after the start-up check has run.
pub(crate) fn rig_with(
    cx: &mut TestAppContext,
    then: Relaunch,
    gate: Option<oneshot::Receiver<()>>,
    fail: bool,
    extra: impl FnOnce(UpdateHooks) -> UpdateHooks,
) -> Rig {
    support::init(cx);
    // The schedule waits for the user's settings, so the rig installs them first.
    cx.update(|cx| settings_store::ensure(Appearance::System, cx));
    let dir = tempfile::tempdir().unwrap();
    let answer = Rc::new(RefCell::new(available("99.0.0")));
    let source = Rc::clone(&answer);
    let checker: Checker = Rc::new(move |_| Task::ready(Ok(source.borrow().clone())));
    let (s, staged) = stager(then, gate, fail);
    let hooks = extra(UpdateHooks::checking(checker).with_stager(s));
    let path = dir.path().join("update-state.json");
    let updates = cx.update(|cx| updates::install(Some(path), hooks, cx));
    cx.run_until_parked();
    Rig {
        _dir: dir,
        updates,
        staged,
        answer,
    }
}

pub(crate) fn rig(
    cx: &mut TestAppContext,
    then: Relaunch,
    gate: Option<oneshot::Receiver<()>>,
    fail: bool,
) -> Rig {
    rig_with(cx, then, gate, fail, |h| h)
}

pub(crate) fn phase(cx: &mut TestAppContext, r: &Rig) -> InstallPhase {
    r.updates.read_with(cx, |u, _| u.install_phase().clone())
}
pub(crate) fn prepare(cx: &mut TestAppContext, r: &Rig) -> Rc<Cell<Option<bool>>> {
    let done = Rc::new(Cell::new(None));
    let sink = Rc::clone(&done);
    r.updates
        .update(cx, |u, cx| u.prepare(move |ok, _| sink.set(Some(ok)), cx));
    cx.run_until_parked();
    done
}
pub(crate) fn armed(cx: &mut TestAppContext, r: &Rig) -> bool {
    r.updates.read_with(cx, |u, _| u.armed())
}

#[gpui_kit::test]
fn preparing_shows_progress_then_waits_ready(cx: &mut TestAppContext) {
    let (go, gate) = oneshot::channel();
    let r = rig(cx, Relaunch::Restart(IMAGE.into()), Some(gate), false);
    assert!(r.updates.read_with(cx, |u, _| u.can_install()));
    let done = prepare(cx, &r);
    assert_eq!(
        phase(cx, &r),
        InstallPhase::Downloading {
            received: 5,
            total: 10
        }
    );
    assert_eq!(done.get(), None);
    go.send(()).unwrap();
    cx.run_until_parked();
    assert_eq!(phase(cx, &r), InstallPhase::Ready);
    assert_eq!(done.get(), Some(true));
    let again = prepare(cx, &r);
    assert_eq!(
        (again.get(), r.staged.get()),
        (Some(true), 1),
        "the release was fetched twice"
    );
}

#[gpui_kit::test]
fn a_failed_download_is_shown_and_never_armed(cx: &mut TestAppContext) {
    let r = rig(cx, Relaunch::Restart(IMAGE.into()), None, true);
    let done = prepare(cx, &r);
    assert_eq!(done.get(), Some(false));
    assert_eq!(phase(cx, &r), InstallPhase::Failed("disk full".into()));
    r.updates.update(cx, |u, cx| u.arm(cx));
    assert!(!armed(cx, &r));
}

#[gpui_kit::test]
fn disarming_keeps_the_release_and_taking_it_clears_both(cx: &mut TestAppContext) {
    let r = rig(cx, Relaunch::Restart(IMAGE.into()), None, false);
    prepare(cx, &r);
    r.updates.update(cx, |u, cx| u.arm(cx));
    assert!(armed(cx, &r));
    r.updates.update(cx, |u, cx| u.disarm(cx));
    assert!(!armed(cx, &r));
    assert_eq!(
        phase(cx, &r),
        InstallPhase::Ready,
        "disarming dropped the release"
    );
    assert!(
        r.updates.update(cx, |u, _| u.take_armed()).is_none(),
        "an unarmed release was taken"
    );
    r.updates.update(cx, |u, cx| u.arm(cx));
    let taken = r.updates.update(cx, |u, _| u.take_armed());
    assert_eq!(
        taken.map(|p| p.version().to_owned()).as_deref(),
        Some("99.0.0")
    );
    assert!(!armed(cx, &r));
}

#[gpui_kit::test]
fn a_newer_offer_disarms_the_prepared_release(cx: &mut TestAppContext) {
    let r = rig(cx, Relaunch::Restart(IMAGE.into()), None, false);
    prepare(cx, &r);
    r.updates.update(cx, |u, cx| u.arm(cx));
    *r.answer.borrow_mut() = available("99.1.0");
    r.updates.update(cx, |u, cx| u.check_now(cx));
    cx.run_until_parked();
    assert!(
        !armed(cx, &r),
        "a release the banner no longer shows stayed armed"
    );
}

#[gpui_kit::test]
fn skipping_the_offer_disarms_and_drops_the_prepared_release(cx: &mut TestAppContext) {
    let r = rig(cx, Relaunch::Restart(IMAGE.into()), None, false);
    prepare(cx, &r);
    r.updates.update(cx, |u, cx| u.arm(cx));
    r.updates.update(cx, |u, cx| u.skip(cx));
    assert!(!armed(cx, &r), "a skipped release stayed armed");
    assert_eq!(phase(cx, &r), InstallPhase::Idle);
    r.updates.update(cx, |u, cx| u.arm(cx));
    assert!(!armed(cx, &r), "the skipped release could be armed again");
}

#[gpui_kit::test]
fn an_up_to_date_check_disarms_and_drops_the_prepared_release(cx: &mut TestAppContext) {
    let r = rig(cx, Relaunch::Restart(IMAGE.into()), None, false);
    prepare(cx, &r);
    r.updates.update(cx, |u, cx| u.arm(cx));
    *r.answer.borrow_mut() = CheckResult::UpToDate;
    r.updates.update(cx, |u, cx| u.check_now(cx));
    cx.run_until_parked();
    assert!(!armed(cx, &r), "a release with no banner stayed armed");
    assert_eq!(phase(cx, &r), InstallPhase::Idle);
}

// Later or Skip during a download cancels it, so a re-offer of the same version never
// arms (and closes the window) on behalf of a download the user turned down.
fn turning_down_cancels_the_download(
    cx: &mut TestAppContext,
    turn_down: fn(&mut Updates, &mut gpui_kit::Context<Updates>),
) {
    let (go, gate) = oneshot::channel();
    let r = rig(cx, Relaunch::Restart(IMAGE.into()), Some(gate), false);
    let done = prepare(cx, &r);
    r.updates.update(cx, turn_down);
    assert_eq!(
        phase(cx, &r),
        InstallPhase::Idle,
        "the download kept running"
    );
    r.updates.update(cx, |u, cx| u.check_now(cx));
    cx.run_until_parked();
    let _ = go.send(());
    cx.run_until_parked();
    assert_eq!(done.get(), None, "the turned-down download reported back");
    assert_eq!(
        phase(cx, &r),
        InstallPhase::Idle,
        "the turned-down download finished"
    );
    r.updates.update(cx, |u, cx| u.arm(cx));
    assert!(!armed(cx, &r), "the turned-down download was kept");
}

#[gpui_kit::test]
fn later_cancels_a_running_download(cx: &mut TestAppContext) {
    turning_down_cancels_the_download(cx, Updates::later);
}

#[gpui_kit::test]
fn skip_cancels_a_running_download(cx: &mut TestAppContext) {
    turning_down_cancels_the_download(cx, Updates::skip);
}
