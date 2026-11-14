//! Closing the window, once a running statement's question is answered Close, cancels,
//! closes the session, then closes; a second close is immediate.
use crate::support;
use crate::support::{LONG, opened, present, wait_real};
use gpui_kit::{AppContext as _, TestAppContext, VisualTestContext};

fn session_label(cx: &mut TestAppContext, o: &support::Opened) -> Option<String> {
    use gpui_kit::test::TestWindowExt as _;
    cx.update_window(o.win, |_, w, cx| {
        w.render_frame(cx);
        w.try_find("status-session")
            .and_then(|e| e.label().map(str::to_owned))
    })
    .unwrap()
}
// These tests pin the session shutdown, so the run's SQL counts as saved and the unsaved-SQL
// question (tests/tabs.rs) stays out of the way.
fn saved(cx: &mut TestAppContext, o: &support::Opened) {
    let tab = o.shell.read_with(cx, |s, _| s.tab().unwrap().clone());
    cx.update(|cx| tab.update(cx, |t, cx| t.mark_saved(cx)));
}
fn running_long(cx: &mut TestAppContext) -> support::Opened {
    let o = opened(cx, "", "", 0);
    let m = o.model(cx);
    o.run(cx, LONG);
    saved(cx, &o);
    wait_real(cx, "the running row", |cx| {
        m.read_with(cx, |m, _| !m.results().is_empty())
    });
    o
}
// Answers Close to the running-statement question (tests/quit_ask.rs) the close raised, then
// steps the executor only until the close begins: parking would advance the test clock past
// QUIT_CAP and remove the window before these tests look at it.
fn confirm(cx: &mut TestAppContext, o: &support::Opened) {
    use gpui_kit::test::TestWindowExt as _;
    wait_real(cx, "the question", |cx| present(cx, o.win, "discard-close"));
    cx.update_window(o.win, |_, w, cx| w.click("discard-close", cx))
        .unwrap();
    while session_label(cx, o).as_deref() != Some("closing sessions") {
        assert!(cx.executor().tick(), "the close never began");
    }
}
fn closed(cx: &mut TestAppContext) -> bool {
    cx.update(|cx| cx.windows().is_empty())
}

#[gpui_kit::test]
fn closing_without_a_tab_closes_at_once(cx: &mut TestAppContext) {
    support::init(cx);
    let files = tempfile::tempdir().unwrap();
    let (win, _) = cx
        .update(|cx| dbento_app::app::start(support::services(files.path()), None, cx))
        .unwrap();
    assert!(VisualTestContext::from_window(win, cx).simulate_close());
}

#[gpui_kit::test]
fn closing_with_a_running_statement_waits_then_closes(cx: &mut TestAppContext) {
    let o = running_long(cx);
    let mut v = VisualTestContext::from_window(o.win, cx);
    assert!(!v.simulate_close(), "the first close asks");
    confirm(cx, &o);
    assert_eq!(session_label(cx, &o).as_deref(), Some("closing sessions"));
    // LONG yields no batch, so only the cancel lets the runner see Close.
    wait_real(cx, "the window to close", closed);
}

#[gpui_kit::test]
fn a_second_close_exits_at_once(cx: &mut TestAppContext) {
    let o = running_long(cx);
    let mut v = VisualTestContext::from_window(o.win, cx);
    assert!(!v.simulate_close());
    confirm(cx, &o);
    let mut v = VisualTestContext::from_window(o.win, cx);
    assert!(v.simulate_close());
}

#[gpui_kit::test]
fn the_title_bar_close_button_takes_the_same_path(cx: &mut TestAppContext) {
    let o = running_long(cx);
    let shell = o.shell.clone();
    cx.update_window(o.win, |_, w, cx| {
        shell.update(cx, |s, cx| s.title_close(w, cx))
    })
    .unwrap();
    confirm(cx, &o);
    assert_eq!(session_label(cx, &o).as_deref(), Some("closing sessions"));
    wait_real(cx, "the window to close", closed);
}

#[gpui_kit::test]
fn a_full_event_channel_does_not_hold_the_close(cx: &mut TestAppContext) {
    use gpui_kit::test::TestWindowExt as _;
    let o = opened(cx, "", "", 0);
    let m = o.model(cx);
    // The pump only reads while the test runs the executor, so clicking Run without
    // parking leaves the runner blocked on a full channel of a long script's events.
    let tab = o.shell.read_with(cx, |s, _| s.tab().unwrap().clone());
    let script = "SELECT 1;\n".repeat(2000);
    cx.update_window(o.win, |_, window, cx| {
        tab.update(cx, |t, cx| t.set_sql(&script, window, cx));
        window.click("run-all", cx);
        tab.update(cx, |t, cx| t.mark_saved(cx));
    })
    .unwrap();
    let events = m.read_with(cx, |m, _| m.events()).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !events.is_full() {
        assert!(
            std::time::Instant::now() < deadline,
            "the channel never filled"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    drop(events);
    let start = std::time::Instant::now();
    // Answering the question would let the pump drain the channel first, so this closes as
    // its Close answer does, while the runner is still blocked.
    let shell = o.shell.clone();
    cx.update_window(o.win, |_, w, cx| {
        shell.update(cx, |s, cx| s.close_unasked(w, cx))
    })
    .unwrap();
    // The test clock never reaches QUIT_CAP, so only a drained channel closes the window.
    wait_real(cx, "the window to close", closed);
    assert!(start.elapsed() < dbento_app::shell::QUIT_CAP);
}

#[gpui_kit::test]
fn nothing_new_starts_while_the_window_closes(cx: &mut TestAppContext) {
    use gpui_kit::test::TestWindowExt as _;
    let o = running_long(cx);
    let m = o.model(cx);
    let mut v = VisualTestContext::from_window(o.win, cx);
    assert!(!v.simulate_close());
    confirm(cx, &o);
    // Neither the Run button nor a direct run may open a session the close does not wait for.
    cx.update_window(o.win, |_, w, cx| w.click("run", cx))
        .unwrap();
    m.update(cx, |m, cx| m.run("SELECT 1".into(), cx));
    assert!(
        m.read_with(cx, |m, _| m.events().is_none()),
        "a new runner started"
    );
    // Read without parking: the session closes quickly, and the window with it.
    let header = cx
        .update_window(o.win, |_, w, cx| {
            w.render_frame(cx);
            w.try_find(("statement-result", 0usize))
                .and_then(|e| e.label().map(str::to_owned))
        })
        .unwrap();
    assert_eq!(header.as_deref(), Some("closing"));
    wait_real(cx, "the window to close", closed);
}

#[gpui_kit::test]
fn the_window_close_waits_for_a_tab_closed_earlier(cx: &mut TestAppContext) {
    use gpui_kit::test::TestWindowExt as _;
    let o = opened(cx, "", "", 0);
    let m = o.model(cx);
    let tab = o.shell.read_with(cx, |s, _| s.tab().unwrap().clone());
    let script = "SELECT 1;\n".repeat(2000);
    cx.update_window(o.win, |_, window, cx| {
        tab.update(cx, |t, cx| t.set_sql(&script, window, cx));
        window.click("run-all", cx);
        tab.update(cx, |t, cx| t.mark_saved(cx));
    })
    .unwrap();
    let events = m.read_with(cx, |m, _| m.events()).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while !events.is_full() {
        assert!(
            std::time::Instant::now() < deadline,
            "the channel never filled"
        );
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    // Close the tab and then the window without parking, so the tab's runner is still blocked.
    // Closing all tabs asks nothing for a run, so no question needs the parking an answer takes.
    let shell = o.shell.clone();
    cx.update_window(o.win, |_, w, cx| {
        shell.update(cx, |s, cx| s.close_tabs_except(None, w, cx))
    })
    .unwrap();
    assert_eq!(
        o.shell
            .read_with(cx, |s, _| (s.tabs().len(), s.pending_closings())),
        (0, 1)
    );
    let mut v = VisualTestContext::from_window(o.win, cx);
    assert!(
        !v.simulate_close(),
        "the close waits for the closed tab's session"
    );
    wait_real(cx, "the runner to end", |_| events.is_closed());
    wait_real(cx, "the window to close", closed);
}

#[tokio::test]
async fn wait_capped_says_whether_the_cap_fired() {
    use dbento_app::shell::wait_capped;
    let short = tokio::time::sleep(std::time::Duration::from_millis(10));
    assert!(wait_capped(std::future::pending::<()>(), short).await);
    assert!(!wait_capped(async {}, std::future::pending::<()>()).await);
}
