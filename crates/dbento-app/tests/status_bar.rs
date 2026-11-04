mod support;
use dbento_app::results_pane::format_rows;
use dbento_app::runner::Event;
use dbento_app::status_bar::format_elapsed;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext, px};
use std::time::Duration;
use support::{MANY, label_of, opened, present, wait_real};

#[test]
fn elapsed_reads_naturally() {
    assert_eq!(format_rows(1000), "1,000 rows"); // the bar reuses the pane's formatting
    assert_eq!(format_elapsed(Duration::ZERO), "0 ms");
    assert_eq!(format_elapsed(Duration::from_millis(999)), "999 ms");
    assert_eq!(format_elapsed(Duration::from_millis(1250)), "1.25 s");
}
#[gpui_kit::test]
fn the_bar_shows_rows_elapsed_and_truncation(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    assert!(
        label_of(cx, o.win, "status-connection")
            .unwrap()
            .starts_with("lite")
    );
    o.model(cx).update(cx, |m, _| m.set_max_rows(Some(1_000)));
    o.run(cx, MANY);
    o.idle(cx);
    // A read cut by its cap parks there; Stop ends it truncated with its rows.
    let m = o.model(cx);
    m.update(cx, |m, _| m.cancel());
    wait_real(cx, "the stop", |cx| {
        m.read_with(cx, |m, _| m.results()[0].store.end().is_some())
    });
    assert_eq!(
        label_of(cx, o.win, "status-rows").as_deref(),
        Some("1,000 rows")
    );
    assert_eq!(
        label_of(cx, o.win, "status-truncated").as_deref(),
        Some("truncated at 1,000 rows")
    );
    let elapsed = label_of(cx, o.win, "footer-elapsed").unwrap();
    assert!(
        elapsed.ends_with(" ms") || elapsed.ends_with(" s"),
        "{elapsed}"
    );
    o.run(cx, "SELECT 1");
    o.idle(cx);
    assert_eq!(label_of(cx, o.win, "status-rows").as_deref(), Some("1 row"));
    assert!(!present(cx, o.win, "status-truncated"));
}
#[gpui_kit::test]
fn a_failed_connect_shows_the_message_and_its_hint(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 1);
    o.run(cx, "SELECT 1");
    o.idle(cx);
    let message = label_of(cx, o.win, "result-message").expect("a message");
    assert!(message.contains("DBENTO_SECRET_NOPG"), "{message}");
    assert!(
        label_of(cx, o.win, "status-connection")
            .unwrap()
            .starts_with("nopg")
    );
}
#[gpui_kit::test]
fn a_stopped_session_says_so_and_the_next_run_recovers(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    o.run(cx, "SELECT 1");
    o.idle(cx);
    let m = o.model(cx);
    m.update(cx, |m, cx| {
        m.apply(Event::Stopped {
            message: "session stopped".into(),
        });
        cx.notify();
    });
    assert_eq!(
        label_of(cx, o.win, "result-message").as_deref(),
        Some("session stopped")
    );
    o.run(cx, "SELECT 5");
    o.idle(cx);
    assert!(!present(cx, o.win, "result-message"));
    assert_eq!(label_of(cx, o.win, "cell-0-0").as_deref(), Some("5"));
}
#[gpui_kit::test]
fn a_narrow_window_keeps_every_figure_inside_the_bar(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    o.model(cx).update(cx, |m, _| m.set_max_rows(Some(1_000)));
    o.run(cx, MANY);
    o.idle(cx);
    // A read cut by its cap parks there; Stop ends it truncated with its rows.
    let m = o.model(cx);
    m.update(cx, |m, _| m.cancel());
    wait_real(cx, "the stop", |cx| {
        m.read_with(cx, |m, _| m.results()[0].store.end().is_some())
    });
    cx.simulate_window_resize(
        o.win,
        gpui_kit::size(gpui_kit::px(520.), gpui_kit::px(600.)),
    );
    assert!(present(cx, o.win, "status-truncated"));
    cx.update_window(o.win, |_, w, cx| {
        w.render_frame(cx);
        let bar = w.find("status-bar").bounds();
        for id in ["status-connection", "status-rows", "status-truncated"] {
            let b = w.find(id).bounds();
            assert!(
                b.right() <= bar.right() && b.size.width > gpui_kit::px(0.),
                "{id} {b:?} outside {bar:?}"
            );
        }
    })
    .unwrap();
}
#[gpui_kit::test]
fn the_dot_shows_a_colour_inherited_from_the_folder(cx: &mut TestAppContext) {
    let o = opened(
        cx,
        "",
        "folder = \"Prod/EU\"\n[folders.\"Prod\"]\ncolor = \"red\"",
        0,
    );
    assert_eq!(label_of(cx, o.win, "status-color").as_deref(), Some("red"));
}
#[gpui_kit::test]
fn with_no_tab_the_bar_says_no_connection_and_ready(cx: &mut TestAppContext) {
    let s = support::started(cx, support::TREE);
    assert_eq!(
        label_of(cx, s.win, "status-connection").as_deref(),
        Some("No connection")
    );
    assert_eq!(
        label_of(cx, s.win, "status-session").as_deref(),
        Some("Ready")
    );
}
#[gpui_kit::test]
fn the_bar_is_28_px_and_its_connection_is_a_22_px_chip(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    cx.update_window(o.win, |_, w, cx| {
        w.render_frame(cx);
        assert_eq!(w.find("status-bar").bounds().size.height, px(28.));
        assert_eq!(w.find("status-connection").bounds().size.height, px(22.));
    })
    .unwrap();
}
#[gpui_kit::test]
fn a_later_failure_hides_the_earlier_statements_rows(cx: &mut TestAppContext) {
    let o = opened(cx, "CREATE TABLE t(x);", "", 0);
    o.run(cx, "SELECT x FROM t; SELECT nope FROM t");
    o.idle(cx);
    assert!(!present(cx, o.win, "status-rows"));
}
