//! A statement that runs before its first rows reads as running: its result tab, footer and
//! status bar say "Running…" with a counting time, its tab in the strip carries a spinner,
//! and an idle toolbar shows Cancel disabled rather than a hole.
use std::time::Duration;

use crate::support::{LONG, Opened, click_id, label_of, opened, present, wait_real};
use dbento_app::results_pane::running_label;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext};

// Starts LONG, which counts without a row until stopped, and waits until it executes.
fn long_running(cx: &mut TestAppContext) -> Opened {
    let o = opened(cx, "", "", 0);
    o.run(cx, LONG);
    let m = o.model(cx);
    wait_real(cx, "the statement executing", |cx| {
        m.read_with(cx, |m, _| m.awaiting_rows(0).is_some())
    });
    o
}

fn stop(cx: &mut TestAppContext, o: &Opened) {
    cx.update_window(o.win, |_, w, cx| w.click("cancel", cx))
        .unwrap();
    o.idle(cx);
}

#[test]
fn the_running_label_counts_tenths_of_a_second() {
    assert_eq!(
        running_label(Duration::from_millis(3_249)),
        "Running\u{2026} 3.2 s"
    );
    assert_eq!(running_label(Duration::ZERO), "Running\u{2026} 0.0 s");
}

#[gpui_kit::test]
fn a_statement_without_rows_yet_counts_its_time(cx: &mut TestAppContext) {
    let o = long_running(cx);
    let m = o.model(cx);
    let first = m.read_with(cx, |m, _| m.awaiting_rows(0)).unwrap();
    std::thread::sleep(Duration::from_millis(50));
    let later = m.read_with(cx, |m, _| m.awaiting_rows(0)).unwrap();
    assert!(later > first, "{first:?} then {later:?}");
    // The pane repaints only while that time counts.
    assert!(m.read_with(cx, |m, _| m.ticking()));
    stop(cx, &o);
    assert_eq!(m.read_with(cx, |m, _| m.awaiting_rows(0)), None);
    assert!(!m.read_with(cx, |m, _| m.ticking()));
}

#[gpui_kit::test]
fn the_result_tab_and_footer_say_running_until_rows_arrive(cx: &mut TestAppContext) {
    let o = long_running(cx);
    let tab = label_of(cx, o.win, ("statement-result", 0usize)).unwrap();
    assert!(tab.starts_with("Running\u{2026} "), "{tab}");
    let footer = label_of(cx, o.win, "footer-rows").unwrap();
    assert!(footer.starts_with("Running\u{2026} "), "{footer}");
    assert!(
        !present(cx, o.win, "footer-elapsed"),
        "the time shows once, in the running text"
    );
    stop(cx, &o);
    let footer = label_of(cx, o.win, "footer-rows");
    assert!(
        footer.as_deref().is_none_or(|f| !f.starts_with("Running")),
        "{footer:?}"
    );
}

#[gpui_kit::test]
fn the_status_bar_does_not_count_zero_rows_while_running(cx: &mut TestAppContext) {
    let o = long_running(cx);
    let rows = label_of(cx, o.win, "status-rows");
    assert!(rows.as_deref() != Some("0 rows"), "{rows:?}");
    stop(cx, &o);
}

#[gpui_kit::test]
fn a_running_tab_shows_a_spinner_in_the_strip_even_behind_another(cx: &mut TestAppContext) {
    let o = long_running(cx);
    assert!(present(cx, o.win, ("tab-running", 0usize)));
    click_id(cx, o.win, "tab-new");
    assert!(
        present(cx, o.win, ("tab-running", 0usize)),
        "in the background"
    );
    assert!(!present(cx, o.win, ("tab-running", 1usize)), "the idle tab");
    click_id(cx, o.win, ("tab", 0usize));
    stop(cx, &o);
    assert!(!present(cx, o.win, ("tab-running", 0usize)));
}

#[gpui_kit::test]
fn an_idle_toolbar_shows_cancel_disabled_in_its_place(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let idle = cx
        .update_window(o.win, |_, w, cx| {
            w.render_frame(cx);
            w.try_find("cancel-idle").map(|e| e.bounds())
        })
        .unwrap()
        .expect("a disabled Cancel while idle");
    o.run(cx, LONG);
    let m = o.model(cx);
    wait_real(cx, "the run", |cx| m.read_with(cx, |m, _| m.running()));
    let live = cx
        .update_window(o.win, |_, w, cx| {
            w.render_frame(cx);
            assert!(w.try_find("cancel-idle").is_none());
            w.find("cancel").bounds()
        })
        .unwrap();
    assert_eq!(live, idle);
    stop(cx, &o);
}
