//! The app's update check: the daily schedule, the manual check, skip and later, the state
//! file and the Settings default.
use crate::support;
use dbento_app::app::AppPaths;
use dbento_app::settings::{self, Settings};
use dbento_app::theme::Appearance;
use dbento_app::updates::{
    self, CHECK_INTERVAL, CheckResult, Checker, Offer, Report, UpdateHooks, Updates, UpdatesEvent,
    check_result, report_text,
};
use dbento_core::update::CheckError;
use gpui_kit::{AppContext as _, Entity, Task, TestAppContext};
use std::cell::{Cell, RefCell};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const T0: u64 = 1_800_000_000;
const DAY: u64 = 24 * 60 * 60;

fn at(secs: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(secs)
}
fn offer(version: &str) -> Offer {
    Offer {
        version: version.into(),
        download_page: "https://example.com/download".into(),
    }
}
/// A checker that answers `answer` every time and counts its calls.
fn checker(answer: Result<CheckResult, String>) -> (Checker, Rc<Cell<usize>>) {
    let calls = Rc::new(Cell::new(0));
    let seen = Rc::clone(&calls);
    let c: Checker = Rc::new(move |_| {
        seen.set(seen.get() + 1);
        Task::ready(answer.clone())
    });
    (c, calls)
}
fn state_path(dir: &Path) -> PathBuf {
    AppPaths::in_dir(dir).update_state
}
fn model(cx: &mut TestAppContext, dir: &Path, hooks: UpdateHooks) -> Entity<Updates> {
    let path = state_path(dir);
    cx.new(|_| Updates::new(Some(path), hooks))
}
fn reports(cx: &mut TestAppContext, u: &Entity<Updates>) -> Rc<RefCell<Vec<Report>>> {
    let seen = Rc::new(RefCell::new(Vec::new()));
    let sink = Rc::clone(&seen);
    cx.update(|cx| {
        cx.subscribe(u, move |_, e: &UpdatesEvent, _| {
            let UpdatesEvent::Reported(r) = e;
            sink.borrow_mut().push(r.clone());
        })
        .detach()
    });
    seen
}
fn due(cx: &mut TestAppContext, u: &Entity<Updates>, enabled: bool, now: u64) {
    u.update(cx, |u, cx| u.check_if_due(enabled, at(now), cx));
    cx.run_until_parked();
}
fn offered(cx: &mut TestAppContext, u: &Entity<Updates>) -> Option<String> {
    u.read_with(cx, |u, _| u.offer().map(|o| o.version.clone()))
}
fn saved(dir: &Path) -> serde_json::Value {
    serde_json::from_str(&std::fs::read_to_string(state_path(dir)).unwrap()).unwrap()
}

#[test]
fn check_for_updates_defaults_on_and_round_trips() {
    let dir = tempfile::tempdir().unwrap();
    let path = AppPaths::in_dir(dir.path()).settings;
    std::fs::write(&path, r#"{"density":"compact"}"#).unwrap();
    assert!(
        settings::load(&path, Appearance::System)
            .settings
            .check_for_updates
    );
    std::fs::write(&path, r#"{"check_for_updates":false}"#).unwrap();
    let off = settings::load(&path, Appearance::System).settings;
    assert!(!off.check_for_updates);
    let json = serde_json::to_value(&off).unwrap();
    assert_eq!(json["check_for_updates"], serde_json::json!(false));
    assert!(Settings::default().check_for_updates);
}

#[test]
fn the_state_file_sits_beside_the_app_state() {
    let p = AppPaths::in_dir(Path::new("/s"));
    assert_eq!(p.update_state, PathBuf::from("/s/update-state.json"));
}

#[gpui_kit::test]
fn an_automatic_check_runs_at_most_once_a_day(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (c, calls) = checker(Ok(CheckResult::UpToDate));
    let u = model(cx, dir.path(), UpdateHooks::checking(c));
    assert_eq!(CHECK_INTERVAL, Duration::from_secs(DAY));
    for (now, expected) in [(T0, 1), (T0 + 3600, 1), (T0 + DAY, 1), (T0 + DAY + 1, 2)] {
        due(cx, &u, true, now);
        assert_eq!(calls.get(), expected, "at +{} s", now - T0);
    }
}

#[gpui_kit::test]
fn the_setting_off_or_a_build_without_a_checker_never_checks(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (c, calls) = checker(Ok(CheckResult::UpToDate));
    let u = model(cx, dir.path(), UpdateHooks::checking(c));
    due(cx, &u, false, T0);
    assert_eq!(calls.get(), 0);
    let none = model(cx, dir.path(), UpdateHooks::none());
    due(cx, &none, true, T0);
    assert!(
        !state_path(dir.path()).exists(),
        "a build without a checker recorded a check"
    );
}

#[gpui_kit::test]
fn a_last_check_in_the_future_does_not_block_one_now(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        state_path(dir.path()),
        format!(r#"{{"last_check":{}}}"#, T0 + 10 * DAY),
    )
    .unwrap();
    let (c, calls) = checker(Ok(CheckResult::UpToDate));
    let u = model(cx, dir.path(), UpdateHooks::checking(c));
    due(cx, &u, true, T0);
    assert_eq!(calls.get(), 1);
}

#[gpui_kit::test]
fn the_last_check_is_saved_and_read_back(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (c, _) = checker(Ok(CheckResult::UpToDate));
    let first = model(cx, dir.path(), UpdateHooks::checking(c));
    due(cx, &first, true, T0);
    assert_eq!(saved(dir.path())["last_check"], serde_json::json!(T0));
    let (c, calls) = checker(Ok(CheckResult::UpToDate));
    let second = model(cx, dir.path(), UpdateHooks::checking(c));
    due(cx, &second, true, T0 + 3600);
    assert_eq!(calls.get(), 0, "the saved check was not honoured");
}

#[gpui_kit::test]
fn a_skipped_release_stays_hidden_until_a_manual_check(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (c, _) = checker(Ok(CheckResult::Available(offer("99.0.0"))));
    let u = model(cx, dir.path(), UpdateHooks::checking(c));
    let seen = reports(cx, &u);
    due(cx, &u, true, T0);
    assert_eq!(offered(cx, &u).as_deref(), Some("99.0.0"));
    assert!(seen.borrow().is_empty(), "an automatic check reported");
    u.update(cx, |u, cx| u.skip(cx));
    cx.run_until_parked();
    assert_eq!(offered(cx, &u), None);
    assert_eq!(
        saved(dir.path())["skipped_version"],
        serde_json::json!("99.0.0")
    );
    due(cx, &u, true, T0 + DAY + 1);
    assert_eq!(offered(cx, &u), None, "a skipped release came back");
    u.update(cx, |u, cx| u.check_now(cx));
    cx.run_until_parked();
    assert_eq!(offered(cx, &u).as_deref(), Some("99.0.0"));
    assert_eq!(*seen.borrow(), [Report::Available("99.0.0".into())]);
}

#[gpui_kit::test]
fn later_hides_the_offer_for_this_run_only(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (c, _) = checker(Ok(CheckResult::Available(offer("99.0.0"))));
    let u = model(cx, dir.path(), UpdateHooks::checking(c));
    u.update(cx, |u, cx| u.check_now(cx));
    cx.run_until_parked();
    u.update(cx, |u, cx| u.later(cx));
    assert_eq!(offered(cx, &u), None);
    assert!(
        saved(dir.path())["skipped_version"].is_null(),
        "later recorded a skip"
    );
    u.update(cx, |u, cx| u.check_now(cx));
    cx.run_until_parked();
    assert_eq!(offered(cx, &u).as_deref(), Some("99.0.0"));
}

#[gpui_kit::test]
fn automatic_errors_stay_quiet_and_manual_ones_are_reported(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let (c, _) = checker(Err("offline".into()));
    let u = model(cx, dir.path(), UpdateHooks::checking(c));
    let seen = reports(cx, &u);
    due(cx, &u, true, T0);
    assert!(seen.borrow().is_empty() && offered(cx, &u).is_none());
    u.update(cx, |u, cx| u.check_now(cx));
    cx.run_until_parked();
    assert_eq!(*seen.borrow(), [Report::Failed("offline".into())]);
    let last = u.read_with(cx, |u, _| u.last_report().cloned());
    assert_eq!(last, Some(Report::Failed("offline".into())));
}

#[gpui_kit::test]
fn a_manual_check_without_a_checker_says_so(cx: &mut TestAppContext) {
    let dir = tempfile::tempdir().unwrap();
    let u = model(cx, dir.path(), UpdateHooks::none());
    let seen = reports(cx, &u);
    u.update(cx, |u, cx| u.check_now(cx));
    cx.run_until_parked();
    assert_eq!(*seen.borrow(), [Report::Disabled]);
}

#[test]
fn reports_read_as_sentences() {
    let v = env!("CARGO_PKG_VERSION");
    assert_eq!(
        report_text(&Report::UpToDate),
        format!("Dbento {v} is up to date.")
    );
    assert_eq!(
        report_text(&Report::Available("9.0.0".into())),
        "Dbento 9.0.0 is available."
    );
    assert_eq!(
        report_text(&Report::Failed("offline".into())),
        "Update check failed: offline"
    );
    assert_eq!(
        report_text(&Report::Disabled),
        "Update checks are not configured for this build."
    );
}

#[test]
fn a_release_without_a_manifest_is_up_to_date() {
    let page = "https://example.com/download";
    assert_eq!(
        check_result(Err(CheckError::Http(404)), page),
        Ok(CheckResult::UpToDate)
    );
    assert_eq!(check_result(Ok(None), page), Ok(CheckResult::UpToDate));
    assert_eq!(
        check_result(Err(CheckError::Http(503)), page),
        Err("the update server answered HTTP 503".to_string())
    );
}

#[gpui_kit::test]
fn install_waits_for_the_settings_and_follows_them(cx: &mut TestAppContext) {
    support::init(cx);
    let dir = tempfile::tempdir().unwrap();
    let (c, calls) = checker(Ok(CheckResult::UpToDate));
    let path = state_path(dir.path());
    let u = cx.update(|cx| updates::install(Some(path), UpdateHooks::checking(c), cx));
    cx.run_until_parked();
    assert_eq!(calls.get(), 0, "checked before the user's setting was read");
    let store = cx.update(|cx| dbento_app::settings_store::install(None, Appearance::System, cx));
    store.update(cx, |s, cx| s.change(cx, |v| v.check_for_updates = false));
    cx.executor().advance_clock(updates::POLL_INTERVAL);
    cx.run_until_parked();
    assert_eq!(calls.get(), 0, "checked with the setting off");
    store.update(cx, |s, cx| s.change(cx, |v| v.check_for_updates = true));
    cx.executor().advance_clock(updates::POLL_INTERVAL);
    cx.run_until_parked();
    assert_eq!(calls.get(), 1, "the schedule never checked");
    let global = cx.update(|cx| updates::global(cx).map(|g| g.entity_id()));
    assert_eq!(global, Some(u.entity_id()));
}
