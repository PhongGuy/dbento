//! Connection state: merged from catalog and tab sessions, drawn on the row and in the bar.
use crate::support;
use crate::support::{TREE, click_id, label_of, launched, opened, present, started, wait_real};
use dbento_app::explorer::{connection_id, part_id};
use dbento_app::link::LinkState::{Connected, Connecting, Failed, Idle};
use gpui_kit::{AnyWindowHandle, TestAppContext};

fn status(cx: &mut TestAppContext, win: AnyWindowHandle, conn: &str) -> Option<String> {
    label_of(cx, win, part_id("connection-status", conn))
}

#[test]
fn merge_prefers_failure_then_connecting_then_connected() {
    let boom = || Failed("boom".to_owned());
    assert_eq!(Idle.merge(Connected), Connected);
    assert_eq!(Connected.merge(Connecting), Connecting);
    assert_eq!(Connecting.merge(boom()), boom());
    assert_eq!(boom().merge(Failed("later".to_owned())), boom());
    assert_eq!(Idle.merge(Idle), Idle);
    assert_eq!(Idle.word(), "not connected");
    assert_eq!(Connecting.word(), "connecting");
    assert_eq!(Connected.word(), "connected");
    assert_eq!(boom().word(), "failed");
}

#[gpui_kit::test]
fn an_open_tab_marks_its_connection_connected(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    wait_real(cx, "lite connected", |cx| {
        status(cx, o.win, "lite").as_deref() == Some("connected")
    });
    assert!(
        !present(cx, o.win, part_id("connection-status", "nopg")),
        "an idle connection draws nothing"
    );
}

#[gpui_kit::test]
fn a_tab_that_cannot_connect_marks_its_connection_failed(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 1);
    wait_real(cx, "nopg failed", |cx| {
        status(cx, o.win, "nopg").is_some_and(|s| s != "connecting")
    });
    let shown = status(cx, o.win, "nopg").unwrap();
    let sidebar = o.shell.read_with(cx, |s, _| s.sidebar().cloned().unwrap());
    assert_eq!(sidebar.read_with(cx, |s, _| s.link("nopg")), Failed(shown));
}

#[gpui_kit::test]
fn a_failed_catalog_load_shows_its_full_error_and_retry(cx: &mut TestAppContext) {
    let o = launched(cx, "", "");
    click_id(cx, o.win, part_id("connection-toggle", "nopg"));
    wait_real(cx, "nopg failed", |cx| {
        status(cx, o.win, "nopg").is_some_and(|s| s != "connecting")
    });
    let shown = status(cx, o.win, "nopg").unwrap();
    assert!(!shown.is_empty());
    let sidebar = o.shell.read_with(cx, |s, _| s.sidebar().cloned().unwrap());
    assert_eq!(sidebar.read_with(cx, |s, _| s.link("nopg")), Failed(shown));
    assert!(present(cx, o.win, ("tree-retry", 0usize)));
}

#[gpui_kit::test]
fn with_no_tab_the_bar_shows_the_selected_connection(cx: &mut TestAppContext) {
    let s = started(cx, TREE);
    assert_eq!(
        label_of(cx, s.win, "status-connection").as_deref(),
        Some("No connection")
    );
    click_id(cx, s.win, connection_id("top"));
    assert_eq!(
        label_of(cx, s.win, "status-connection").as_deref(),
        Some("top \u{B7} not connected")
    );
    // TREE names `t.db` without creating it; an empty file is a valid SQLite database.
    std::fs::File::create(s.dir.path().join("t.db")).unwrap();
    click_id(cx, s.win, part_id("connection-toggle", "top"));
    wait_real(cx, "top connected", |cx| {
        label_of(cx, s.win, "status-connection").as_deref() == Some("top \u{B7} connected")
    });
}

// A tab's model reports `Connected` as read-only, the way a SurrealDB VIEWER login does
// whatever its config says; the cancel handle comes from a real session on the same file.
fn report_read_only(cx: &mut TestAppContext, o: &support::Opened) {
    let m = o.model(cx);
    wait_real(cx, "lite connected", |cx| {
        m.read_with(cx, |m, _| m.read_only().is_some())
    });
    let conn = o.fx.ws.connection("lite").unwrap().clone();
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let session = rt
        .block_on(dbento_core::connect(
            &o.fx.ws,
            &conn,
            None,
            dbento_core::SessionOptions::default(),
        ))
        .unwrap();
    m.update(cx, |m, cx| {
        m.apply(dbento_app::runner::Event::Connected {
            read_only: true,
            level: Some(dbento_core::ReadOnlyEnforcement::Database),
            engine: dbento_core::Engine::Sqlite,
            cancel: session.cancel_handle(),
        });
        cx.notify();
    });
}

#[gpui_kit::test]
fn a_read_only_config_draws_the_lock_before_any_session(cx: &mut TestAppContext) {
    let o = launched(cx, "", "read_only = true");
    assert!(present(cx, o.win, part_id("connection-lock", "lite")));
    assert!(!present(cx, o.win, part_id("connection-lock", "nopg")));
}

#[gpui_kit::test]
fn a_session_that_reports_read_only_draws_the_lock_beside_the_badge(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    assert!(!present(cx, o.win, part_id("connection-lock", "lite")));
    report_read_only(cx, &o);
    assert!(
        present(cx, o.win, "read-only-badge"),
        "the badge reads the session"
    );
    assert!(
        present(cx, o.win, part_id("connection-lock", "lite")),
        "the lock reads the same session"
    );
    assert!(!present(cx, o.win, part_id("connection-lock", "nopg")));
}
