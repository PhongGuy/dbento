//! The update banner in the shell, the Settings section and the manual check's toast.
use crate::support::{self, LONG, click_id, label_of, present, wait_real};
use dbento_app::keymap::{self, CheckForUpdates};
use dbento_app::palette::PaletteCommand;
use dbento_app::settings_dialog::SettingsView;
use dbento_app::settings_store;
use dbento_app::theme::Appearance;
use dbento_app::updates::{
    self, CheckResult, Checker, Notes, NotesState, Offer, Prepared, Progress, Stager, UpdateHooks,
    Updates,
};
use dbento_core::update::{Relaunch, ReleaseNotes, release_notes_url};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    AnyWindowHandle, App, AppContext as _, Bounds, Entity, Pixels, Task, TestAppContext,
    WindowBounds, WindowOptions, point, px, size,
};
use std::rc::Rc;

fn checker() -> Checker {
    Rc::new(|_| {
        Task::ready(Ok(CheckResult::Available(Offer {
            version: "99.0.0".into(),
            download_page: "https://example.com/download".into(),
        })))
    })
}
/// A stager that reports half the download and never finishes.
fn stalled() -> Stager {
    Rc::new(|cx: &mut App, _v: String, mut progress: Progress| {
        progress(5, 10);
        cx.background_spawn(futures_util::future::pending::<Result<Prepared, String>>())
    })
}
/// A stager whose release is ready at once and installs nothing.
fn ready() -> Stager {
    Rc::new(|_: &mut App, v: String, _: Progress| {
        Task::ready(Ok(Prepared::relaunch_only(
            v,
            Relaunch::Restart("/a/Dbento.AppImage".into()),
        )))
    })
}

struct Open {
    files: tempfile::TempDir,
    win: AnyWindowHandle,
    updates: Entity<Updates>,
}
fn open(cx: &mut TestAppContext, stager: Option<Stager>) -> Open {
    support::init(cx);
    // The schedule waits for the user's settings, so they exist before the first tick.
    cx.update(|cx| settings_store::ensure(Appearance::System, cx));
    let files = tempfile::tempdir().unwrap();
    let mut hooks = UpdateHooks::checking(checker());
    if let Some(s) = stager {
        hooks = hooks.with_stager(s);
    }
    let path = files.path().join("update-state.json");
    let updates = cx.update(|cx| updates::install(Some(path), hooks, cx));
    let (win, _) = cx
        .update(|cx| dbento_app::app::start(support::services(files.path()), None, cx))
        .unwrap();
    cx.run_until_parked();
    Open {
        files,
        win,
        updates,
    }
}

#[gpui_kit::test]
fn the_banner_offers_the_release_and_its_links(cx: &mut TestAppContext) {
    let o = open(cx, None);
    assert_eq!(
        label_of(cx, o.win, "update-banner-text").as_deref(),
        Some("Dbento 99.0.0 is available")
    );
    for id in [
        "update-notes",
        "update-download-page",
        "update-later",
        "update-skip",
    ] {
        assert!(present(cx, o.win, id), "{id} missing");
    }
    assert!(
        !present(cx, o.win, "update-install"),
        "a source build offered to install"
    );
    click_id(cx, o.win, "update-notes");
    assert_eq!(cx.opened_url(), Some(release_notes_url("99.0.0")));
    click_id(cx, o.win, "update-download-page");
    assert_eq!(
        cx.opened_url().as_deref(),
        Some("https://example.com/download")
    );
}

#[gpui_kit::test]
fn later_and_skip_hide_the_banner(cx: &mut TestAppContext) {
    let o = open(cx, None);
    click_id(cx, o.win, "update-later");
    assert!(!present(cx, o.win, "update-banner"));
    o.updates.update(cx, |u, cx| u.check_now(cx));
    assert!(present(cx, o.win, "update-banner"));
    click_id(cx, o.win, "update-skip");
    assert!(!present(cx, o.win, "update-banner"));
    let text = std::fs::read_to_string(o.files.path().join("update-state.json")).unwrap();
    let saved: serde_json::Value = serde_json::from_str(&text).unwrap();
    assert_eq!(saved["skipped_version"], serde_json::json!("99.0.0"));
}

#[gpui_kit::test]
fn download_and_restart_shows_progress(cx: &mut TestAppContext) {
    let o = open(cx, Some(stalled()));
    assert!(present(cx, o.win, "update-install"));
    click_id(cx, o.win, "update-install");
    assert_eq!(
        label_of(cx, o.win, "update-progress").as_deref(),
        Some("Downloading 50%")
    );
}

#[gpui_kit::test]
fn an_armed_release_says_when_it_installs_and_can_be_cancelled(cx: &mut TestAppContext) {
    let o = open(cx, Some(ready()));
    o.updates.update(cx, |u, cx| u.prepare(|_, _| {}, cx));
    cx.run_until_parked();
    o.updates.update(cx, |u, cx| u.arm(cx));
    assert_eq!(
        label_of(cx, o.win, "update-armed").as_deref(),
        Some("Dbento 99.0.0 installs when you quit")
    );
    assert!(!present(cx, o.win, "update-install"));
    click_id(cx, o.win, "update-cancel");
    assert!(!o.updates.read_with(cx, |u, _| u.armed()));
    assert!(present(cx, o.win, "update-install"));
}

#[gpui_kit::test]
fn check_for_updates_reports_in_a_toast(cx: &mut TestAppContext) {
    let o = open(cx, None);
    cx.update_window(o.win, |_, w, cx| {
        w.dispatch_action(Box::new(CheckForUpdates), cx)
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(
        label_of(cx, o.win, "notice").as_deref(),
        Some("Dbento 99.0.0 is available.")
    );
}

#[test]
fn check_for_updates_is_in_the_palette_and_the_keymap() {
    assert!(PaletteCommand::ALL.contains(&PaletteCommand::CheckForUpdates));
    assert_eq!(PaletteCommand::CheckForUpdates.label(), "Check for updates");
    let row = keymap::bindings()
        .into_iter()
        .find(|b| b.id == "app.check_for_updates")
        .expect("no keymap row");
    assert_eq!(
        (row.keys, row.context, row.label),
        (keymap::CHECK_UPDATES_KEYS, None, Some("Check for updates"))
    );
}

#[gpui_kit::test]
fn settings_toggle_the_check_and_check_now(cx: &mut TestAppContext) {
    support::init(cx);
    let files = tempfile::tempdir().unwrap();
    let store = cx.update(|cx| settings_store::install(None, Appearance::System, cx));
    let path = files.path().join("update-state.json");
    cx.update(|cx| updates::install(Some(path), UpdateHooks::checking(checker()), cx));
    let (win, _) = cx
        .update(|cx| {
            gpui_kit::open_window(WindowOptions::default(), cx, |w, cx| {
                cx.new(|cx| SettingsView::new(w, cx))
            })
        })
        .unwrap();
    cx.run_until_parked();
    assert!(!present(cx, win, "settings-update-status"));
    click_id(cx, win, "settings-check-updates");
    assert!(!store.read_with(cx, |s, _| s.get().check_for_updates));
    click_id(cx, win, "settings-check-now");
    assert_eq!(
        label_of(cx, win, "settings-update-status").as_deref(),
        Some("Dbento 99.0.0 is available.")
    );
}

#[gpui_kit::test]
fn a_close_the_user_keeps_open_leaves_the_release_armed(cx: &mut TestAppContext) {
    let o = support::opened(cx, "CREATE TABLE t(id INTEGER PRIMARY KEY);", "", 0);
    let hooks = UpdateHooks::checking(checker()).with_stager(ready());
    let updates = cx.update(|cx| updates::install(None, hooks, cx));
    updates.update(cx, |u, cx| u.check_now(cx));
    // A running statement makes the close ask first; its SQL counts as saved.
    let m = o.model(cx);
    o.run(cx, LONG);
    let tab = o.shell.read_with(cx, |s, _| s.tab().unwrap().clone());
    cx.update(|cx| tab.update(cx, |t, cx| t.mark_saved(cx)));
    wait_real(cx, "the running row", |cx| {
        m.read_with(cx, |m, _| m.running())
    });
    click_id(cx, o.win, "update-install");
    wait_real(cx, "the close question", |cx| {
        present(cx, o.win, "confirm-dialog")
    });
    click_id(cx, o.win, "discard-keep");
    assert!(
        !cx.update(|cx| cx.windows().is_empty()),
        "the window closed"
    );
    assert!(updates.read_with(cx, |u, _| u.armed()), "Keep disarmed it");
    m.update(cx, |m, _| m.cancel());
    o.idle(cx);
}

const LONG_FAILURE: &str = "could not reach the update server: error connecting to \
    updates.example.com on every address it resolved to, after trying each of them twice";

fn bounds_of(cx: &mut TestAppContext, win: AnyWindowHandle, id: &'static str) -> Bounds<Pixels> {
    cx.run_until_parked();
    cx.update_window(win, |_, w, cx| {
        w.render_frame(cx);
        w.find(id).bounds()
    })
    .unwrap()
}

// A long failure wraps inside the 640 px dialog, and Check now matches the section's
// default-size buttons, such as Open keymap file.
#[gpui_kit::test]
fn a_long_failure_wraps_in_settings_beside_a_default_size_button(cx: &mut TestAppContext) {
    support::init(cx);
    let files = tempfile::tempdir().unwrap();
    cx.update(|cx| settings_store::install(None, Appearance::System, cx));
    let failing: Checker = Rc::new(|_| Task::ready(Err(LONG_FAILURE.to_string())));
    let path = files.path().join("update-state.json");
    cx.update(|cx| updates::install(Some(path), UpdateHooks::checking(failing), cx));
    let options = WindowOptions {
        window_bounds: Some(WindowBounds::Windowed(Bounds::new(
            point(px(0.), px(0.)),
            size(px(640.), px(1000.)),
        ))),
        ..Default::default()
    };
    let (win, _) = cx
        .update(|cx| {
            gpui_kit::open_window(options, cx, |w, cx| cx.new(|cx| SettingsView::new(w, cx)))
        })
        .unwrap();
    cx.run_until_parked();
    click_id(cx, win, "settings-check-now");
    let status = bounds_of(cx, win, "settings-update-status");
    assert!(
        status.right() <= px(640.),
        "the failure ran past the dialog: {status:?}"
    );
    let check = bounds_of(cx, win, "settings-check-now");
    assert!(
        status.size.height > check.size.height,
        "the failure did not wrap: {status:?}"
    );
    let keymap = bounds_of(cx, win, "settings-keymap-open");
    assert_eq!(
        check.size.height, keymap.size.height,
        "Check now is not the section's size"
    );
}

#[gpui_kit::test]
fn a_narrow_window_wraps_the_banner_and_a_wide_one_keeps_one_row(cx: &mut TestAppContext) {
    let o = open(cx, None);
    for (width, one_row) in [(1280., true), (560., false)] {
        cx.simulate_window_resize(o.win, size(px(width), px(800.)));
        cx.run_until_parked();
        let row = bounds_of(cx, o.win, "update-banner");
        let last = bounds_of(cx, o.win, "update-skip");
        assert!(
            row.contains(&last.origin) && row.contains(&last.bottom_right()),
            "at {width} px Skip sits inside the row: {last:?} in {row:?}"
        );
        assert_eq!(
            row.size.height == px(32.),
            one_row,
            "at {width} px the row is {:?} high",
            row.size.height
        );
    }
}

const NOTES: &str = r#"{"schema":1,"releases":[{"version":"99.0.0","previous":"98.0.0","date":"2026-12-01","intro":"Big one.","groups":[{"kind":"features","title":"New features","items":[{"text":"Search fast","pr":12}]}]}]}"#;

fn notes_hook(result: Result<Option<&'static str>, &'static str>) -> Notes {
    Rc::new(move |_: &mut App, _version: String| {
        Task::ready(match result {
            Ok(Some(doc)) => Ok(Some(ReleaseNotes::parse(doc.as_bytes()).unwrap())),
            Ok(None) => Ok(None),
            Err(e) => Err(e.to_owned()),
        })
    })
}

fn open_with_notes(cx: &mut TestAppContext, notes: Notes) -> Open {
    support::init(cx);
    cx.update(|cx| settings_store::ensure(Appearance::System, cx));
    let files = tempfile::tempdir().unwrap();
    let hooks = UpdateHooks::checking(checker()).with_notes(notes);
    let path = files.path().join("update-state.json");
    let updates = cx.update(|cx| updates::install(Some(path), hooks, cx));
    let (win, _) = cx
        .update(|cx| dbento_app::app::start(support::services(files.path()), None, cx))
        .unwrap();
    cx.run_until_parked();
    Open {
        files,
        win,
        updates,
    }
}

#[gpui_kit::test]
fn whats_new_opens_the_grouped_notes(cx: &mut TestAppContext) {
    let o = open_with_notes(cx, notes_hook(Ok(Some(NOTES))));
    assert!(present(cx, o.win, "update-whats-new"));
    click_id(cx, o.win, "update-whats-new");
    assert!(present(cx, o.win, "update-notes-dialog"));
    assert!(present(cx, o.win, "update-notes-scroll"));
    for (id, want) in [
        ("update-notes-release-0", "Dbento 99.0.0 (2026-12-01)"),
        ("update-notes-intro-0", "Big one."),
        ("update-notes-group-0-0", "New features"),
        ("update-notes-item-0-0-0", "• Search fast (#12)"),
        (
            "update-notes-older",
            "Older changes are on the release page",
        ),
    ] {
        assert_eq!(label_of(cx, o.win, id).as_deref(), Some(want), "{id}");
    }
    click_id(cx, o.win, "update-notes-releases");
    assert_eq!(
        cx.opened_url().as_deref(),
        Some(dbento_core::update::RELEASES_URL)
    );
    click_id(cx, o.win, "update-notes-close");
    assert!(!present(cx, o.win, "update-notes-dialog"));
    assert!(present(cx, o.win, "update-banner"));
}

#[gpui_kit::test]
fn notes_that_fail_say_so_and_keep_the_offer(cx: &mut TestAppContext) {
    let o = open_with_notes(cx, notes_hook(Err("bad hash")));
    assert!(matches!(
        o.updates.read_with(cx, |u, _| u.notes_state().clone()),
        NotesState::Failed(_)
    ));
    click_id(cx, o.win, "update-whats-new");
    assert_eq!(
        label_of(cx, o.win, "update-notes-failed").as_deref(),
        Some("Release notes couldn't be loaded")
    );
    assert!(o.updates.read_with(cx, |u, _| u.offer().is_some()));
    assert_eq!(
        label_of(cx, o.win, "update-banner-text").as_deref(),
        Some("Dbento 99.0.0 is available")
    );
}

#[gpui_kit::test]
fn a_release_without_notes_shows_no_whats_new(cx: &mut TestAppContext) {
    let o = open_with_notes(cx, notes_hook(Ok(None)));
    assert!(present(cx, o.win, "update-banner"));
    assert!(!present(cx, o.win, "update-whats-new"));
}

#[gpui_kit::test]
fn no_notes_hook_shows_no_whats_new(cx: &mut TestAppContext) {
    let o = open(cx, None);
    assert!(present(cx, o.win, "update-banner"));
    assert!(
        !present(cx, o.win, "update-whats-new"),
        "no hook still offered notes"
    );
}

// The hook resolves only after the dialog is open, so the Loading dialog must repaint when the notes land.
#[gpui_kit::test]
fn an_open_dialog_shows_notes_that_arrive_later(cx: &mut TestAppContext) {
    let (tx, rx) = async_channel::bounded::<()>(1);
    let hook: Notes = Rc::new(move |cx: &mut App, _version: String| {
        let rx = rx.clone();
        cx.background_spawn(async move {
            let _ = rx.recv().await;
            Ok(Some(ReleaseNotes::parse(NOTES.as_bytes()).unwrap()))
        })
    });
    let o = open_with_notes(cx, hook);
    assert_eq!(
        o.updates.read_with(cx, |u, _| u.notes_state().clone()),
        NotesState::Loading
    );
    click_id(cx, o.win, "update-whats-new");
    assert!(present(cx, o.win, "update-notes-loading"));
    assert!(!present(cx, o.win, "update-notes-release-0"));
    tx.try_send(()).unwrap();
    cx.run_until_parked();
    assert_eq!(
        label_of(cx, o.win, "update-notes-release-0").as_deref(),
        Some("Dbento 99.0.0 (2026-12-01)")
    );
    assert!(!present(cx, o.win, "update-notes-loading"));
}

#[gpui_kit::test]
fn skip_clears_the_notes(cx: &mut TestAppContext) {
    let o = open_with_notes(cx, notes_hook(Ok(Some(NOTES))));
    assert!(matches!(
        o.updates.read_with(cx, |u, _| u.notes_state().clone()),
        NotesState::Loaded(_)
    ));
    click_id(cx, o.win, "update-skip");
    assert_eq!(
        o.updates.read_with(cx, |u, _| u.notes_state().clone()),
        NotesState::Unavailable
    );
}

#[gpui_kit::test]
fn escape_closes_whats_new(cx: &mut TestAppContext) {
    let o = open_with_notes(cx, notes_hook(Ok(Some(NOTES))));
    click_id(cx, o.win, "update-whats-new");
    assert!(present(cx, o.win, "update-notes-dialog"));
    cx.update_window(o.win, |_, w, cx| w.press("escape", cx))
        .unwrap();
    assert!(
        !present(cx, o.win, "update-notes-dialog"),
        "Escape left it open"
    );
}
