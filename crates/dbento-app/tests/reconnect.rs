use crate::support;
use std::sync::Arc;

use crate::support::{LONG, label_of, opened, present, reload, rewrite, wait_real};
use dbento_app::app::AppPaths;
use dbento_app::query_tab::ConnState;
use dbento_core::{History, HistoryFilter, HistoryOutcome};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext};

fn tab_state(cx: &mut TestAppContext, o: &support::Opened) -> ConnState {
    o.shell
        .read_with(cx, |s, cx| s.tab().unwrap().read(cx).conn_state())
}

fn click(cx: &mut TestAppContext, o: &support::Opened, id: &'static str) {
    cx.update_window(o.win, |_, w, cx| w.click(id, cx)).unwrap();
    cx.run_until_parked();
}

fn reconnect(cx: &mut TestAppContext, o: &support::Opened) {
    click(cx, o, "conn-reconnect");
    wait_real(cx, "the question", |cx| present(cx, o.win, "reconnect-ok"));
    click(cx, o, "reconnect-ok");
}

#[gpui_kit::test]
fn a_colour_change_shows_the_band_without_a_chip(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    assert!(!present(cx, o.win, "production-band"));
    rewrite(&o.fx, "color = \"red\"", "");
    reload(cx, o.win);
    assert!(
        present(cx, o.win, "production-band"),
        "the open tab turns production at once"
    );
    assert!(!present(cx, o.win, "conn-chip"));
    assert_eq!(tab_state(cx, &o), ConnState::Current);
}

#[gpui_kit::test]
fn a_session_change_shows_the_chip_and_keeps_the_old_settings(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    rewrite(&o.fx, "statement_timeout = \"5s\"", "");
    reload(cx, o.win);
    assert_eq!(
        label_of(cx, o.win, "conn-chip").as_deref(),
        Some("Settings changed")
    );
    assert!(present(cx, o.win, "conn-reconnect"));
    let model = o.model(cx);
    let old = model.read_with(cx, |m, _| {
        m.spec()
            .workspace
            .connection("lite")
            .unwrap()
            .statement_timeout()
    });
    assert_eq!(old, None, "the runner keeps the settings it started with");
    o.run(cx, "SELECT 1");
    o.idle(cx);
    assert_eq!(
        model.read_with(cx, |m, _| m.error().map(str::to_owned)),
        None
    );
    assert_eq!(
        tab_state(cx, &o),
        ConnState::Changed {
            still_read_write: false
        }
    );
}

#[gpui_kit::test]
fn turning_read_only_on_says_the_tab_is_still_read_write(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    rewrite(&o.fx, "read_only = true", "");
    reload(cx, o.win);
    assert_eq!(
        label_of(cx, o.win, "conn-chip").as_deref(),
        Some("Settings changed: still read-write until you reconnect")
    );
    assert!(
        !present(cx, o.win, "read-only-badge"),
        "the badge describes the session"
    );
}

#[gpui_kit::test]
fn turning_read_only_off_keeps_the_badge_until_reconnect(cx: &mut TestAppContext) {
    let o = opened(cx, "", "read_only = true", 0);
    assert!(present(cx, o.win, "read-only-badge"));
    rewrite(&o.fx, "", "");
    reload(cx, o.win);
    assert!(
        present(cx, o.win, "read-only-badge"),
        "the session is still read-only"
    );
    assert_eq!(
        label_of(cx, o.win, "conn-chip").as_deref(),
        Some("Settings changed")
    );
    reconnect(cx, &o);
    wait_real(cx, "the badge going", |cx| {
        !present(cx, o.win, "read-only-badge")
    });
    assert!(!present(cx, o.win, "conn-chip"));
}

#[gpui_kit::test]
fn a_gone_connection_says_so_without_reconnect(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    std::fs::write(
        o.fx.dir.path().join("dbento.toml"),
        "dbento = 1\n[connections.other]\nengine = \"sqlite\"\npath = \"data/app.db\"\n",
    )
    .unwrap();
    reload(cx, o.win);
    assert_eq!(
        label_of(cx, o.win, "conn-chip").as_deref(),
        Some("Not in dbento.toml")
    );
    assert!(!present(cx, o.win, "conn-reconnect"));
    assert_eq!(tab_state(cx, &o), ConnState::Gone);
    assert_eq!(
        o.shell.read_with(cx, |s, _| s.tabs().len()),
        1,
        "the tab stays open"
    );
}

#[gpui_kit::test]
fn reconnect_asks_then_restarts_the_runner_on_the_new_settings(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    o.run(cx, "SELECT 1");
    o.idle(cx);
    let model = o.model(cx);
    assert_eq!(model.read_with(cx, |m, _| m.read_only()), Some(false));
    rewrite(&o.fx, "read_only = true", "");
    reload(cx, o.win);

    click(cx, &o, "conn-reconnect");
    wait_real(cx, "the question", |cx| {
        present(cx, o.win, "reconnect-cancel")
    });
    click(cx, &o, "reconnect-cancel");
    assert!(
        present(cx, o.win, "conn-chip"),
        "cancel keeps the old session"
    );

    reconnect(cx, &o);
    wait_real(cx, "the read-only session", |cx| {
        model.read_with(cx, |m, _| m.read_only() == Some(true))
    });
    assert!(!present(cx, o.win, "conn-chip"));
    assert!(present(cx, o.win, "read-only-badge"));
    assert_eq!(tab_state(cx, &o), ConnState::Current);
    assert!(!model.read_with(cx, |m, _| m.closed()));
    wait_real(cx, "the old runner", |cx| {
        o.shell.read_with(cx, |s, _| s.pending_closings() == 0)
    });
}

#[gpui_kit::test]
fn the_status_bar_names_the_session_target_and_the_new_colour(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    std::fs::copy(
        o.fx.dir.path().join("data/app.db"),
        o.fx.dir.path().join("data/other.db"),
    )
    .unwrap();
    std::fs::write(
        o.fx.dir.path().join("dbento.toml"),
        "dbento = 1\n[connections.lite]\nengine = \"sqlite\"\npath = \"data/other.db\"\ncolor = \"red\"\n",
    )
    .unwrap();
    reload(cx, o.win);
    assert_eq!(
        tab_state(cx, &o),
        ConnState::Changed {
            still_read_write: false
        }
    );
    assert_eq!(
        label_of(cx, o.win, "status-connection").as_deref(),
        Some("lite \u{B7} data/app.db"),
        "the bar describes the session until reconnect"
    );
    assert_eq!(label_of(cx, o.win, "status-color").as_deref(), Some("red"));
}

#[gpui_kit::test]
fn a_gone_connection_keeps_the_tab_figures_in_the_status_bar(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    o.run(cx, "SELECT 1");
    o.idle(cx);
    std::fs::write(
        o.fx.dir.path().join("dbento.toml"),
        "dbento = 1\n[connections.other]\nengine = \"sqlite\"\npath = \"data/app.db\"\n",
    )
    .unwrap();
    reload(cx, o.win);
    assert_eq!(tab_state(cx, &o), ConnState::Gone);
    assert_eq!(
        label_of(cx, o.win, "status-connection").as_deref(),
        Some("lite \u{B7} data/app.db")
    );
    assert_eq!(label_of(cx, o.win, "status-rows").as_deref(), Some("1 row"));
    assert!(present(cx, o.win, "footer-elapsed"));
}

#[gpui_kit::test]
fn reconnect_cuts_a_running_statement_and_the_new_session_runs(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let model = o.model(cx);
    o.run(cx, LONG);
    wait_real(cx, "the statement to start", |cx| {
        model.read_with(cx, |m, _| m.running() && !m.results().is_empty())
    });
    let ws = Arc::clone(&o.fx.ws);
    let closing = model.update(cx, |m, cx| m.reconnect(ws, cx));
    assert!(!model.read_with(cx, |m, _| m.running()), "the old run ends");
    let path = AppPaths::in_dir(o.files.path()).history;
    let entries = || {
        History::open(&path)
            .map(|h| h.recent(&HistoryFilter::default(), 10))
            .unwrap_or_default()
    };
    wait_real(cx, "the cut statement's entry", |_| !entries().is_empty());
    assert_eq!(entries()[0].outcome(), &HistoryOutcome::Cancelled);
    drop(closing);
    o.run(cx, "SELECT 1");
    o.idle(cx);
    assert_eq!(
        model.read_with(cx, |m, _| m.error().map(str::to_owned)),
        None
    );
    assert_eq!(label_of(cx, o.win, "status-rows").as_deref(), Some("1 row"));
}
