//! Quit, window close, disconnect and a workspace switch ask first while a statement runs,
//! naming its tab.
mod support;
use gpui_kit::{AppContext as _, TestAppContext, VisualTestContext};
use support::{LONG, Opened, click_id, label_of, opened, present, wait_real};

const T: &str = "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT); \
                 INSERT INTO t VALUES (1, 'b'), (2, 'a');";

// The run's SQL counts as saved, so only the running statement can raise the question.
fn run_long(cx: &mut TestAppContext, o: &Opened) {
    let m = o.model(cx);
    o.run(cx, LONG);
    let tab = o.shell.read_with(cx, |s, _| s.tab().unwrap().clone());
    cx.update(|cx| tab.update(cx, |t, cx| t.mark_saved(cx)));
    wait_real(cx, "the running row", |cx| {
        m.read_with(cx, |m, _| !m.results().is_empty() && m.running())
    });
}
fn running_long(cx: &mut TestAppContext) -> Opened {
    let o = opened(cx, T, "", 0);
    run_long(cx, &o);
    o
}
fn asked(cx: &mut TestAppContext, o: &Opened) -> Option<String> {
    wait_real(cx, "the question", |cx| {
        present(cx, o.win, "confirm-dialog")
    });
    label_of(cx, o.win, "confirm-detail")
}
fn closed(cx: &mut TestAppContext) -> bool {
    cx.update(|cx| cx.windows().is_empty())
}
fn quit(cx: &mut TestAppContext, o: &Opened) {
    let shell = o.shell.clone();
    cx.update_window(o.win, |_, w, cx| {
        shell.update(cx, |s, cx| s.title_close(w, cx))
    })
    .unwrap();
    cx.run_until_parked();
}
fn disconnect(cx: &mut TestAppContext, o: &Opened, conn: &str) {
    let shell = o.shell.clone();
    cx.update_window(o.win, |_, w, cx| {
        shell.update(cx, |s, cx| s.disconnect(conn, w, cx))
    })
    .unwrap();
    cx.run_until_parked();
}

#[gpui_kit::test]
fn the_window_close_asks_while_a_statement_runs_and_keep_keeps_it_running(cx: &mut TestAppContext) {
    let o = running_long(cx);
    let m = o.model(cx);
    let mut v = VisualTestContext::from_window(o.win, cx);
    assert!(!v.simulate_close(), "the close waits for the answer");
    assert_eq!(
        asked(cx, &o).as_deref(),
        Some("Closing Dbento cancels the statement running in Query 1.")
    );
    click_id(cx, o.win, "discard-keep");
    assert!(!closed(cx));
    assert!(m.read_with(cx, |m, _| m.running()), "Keep cancels nothing");
    let mut v = VisualTestContext::from_window(o.win, cx);
    assert!(!v.simulate_close(), "a second close asks again");
    asked(cx, &o);
    click_id(cx, o.win, "discard-close");
    wait_real(cx, "the window to close", closed);
}

#[gpui_kit::test]
fn quit_asks_while_a_statement_runs_and_close_cancels_it(cx: &mut TestAppContext) {
    let o = running_long(cx);
    quit(cx, &o);
    assert_eq!(
        asked(cx, &o).as_deref(),
        Some("Closing Dbento cancels the statement running in Query 1.")
    );
    assert!(!closed(cx));
    click_id(cx, o.win, "discard-close");
    wait_real(cx, "the window to close", closed);
}

#[gpui_kit::test]
fn a_statement_ending_while_the_question_is_open_still_closes(cx: &mut TestAppContext) {
    let o = running_long(cx);
    let m = o.model(cx);
    quit(cx, &o);
    asked(cx, &o);
    m.update(cx, |m, _| m.cancel());
    wait_real(cx, "the run to end", |cx| {
        m.read_with(cx, |m, _| !m.running())
    });
    click_id(cx, o.win, "discard-close");
    // Proceeding needs no second question once the statement has ended.
    wait_real(cx, "the window to close", closed);
}

#[gpui_kit::test]
fn the_question_names_every_running_tab(cx: &mut TestAppContext) {
    let o = running_long(cx);
    let shell = o.shell.clone();
    cx.update_window(o.win, |_, w, cx| {
        shell.update(cx, |s, cx| s.open_connection(0, w, cx))
    })
    .unwrap();
    run_long(cx, &o);
    quit(cx, &o);
    assert_eq!(
        asked(cx, &o).as_deref(),
        Some("Closing Dbento cancels the statements running in Query 1 and Query 2.")
    );
    click_id(cx, o.win, "discard-keep");
    cx.update_window(o.win, |_, w, cx| {
        shell.update(cx, |s, cx| s.open_connection(0, w, cx))
    })
    .unwrap();
    run_long(cx, &o);
    quit(cx, &o);
    assert_eq!(
        asked(cx, &o).as_deref(),
        Some("Closing Dbento cancels the statements running in Query 1 and 2 more tabs.")
    );
    click_id(cx, o.win, "discard-close");
    wait_real(cx, "the window to close", closed);
}

#[gpui_kit::test]
fn staged_edits_and_a_running_statement_share_one_question(cx: &mut TestAppContext) {
    let o = opened(cx, T, "", 0);
    o.open_relation(cx, "t", dbento_core::RelationKind::Table);
    o.wait_browse(cx, "SELECT *\nFROM main.t\nORDER BY id ASC");
    let staged = o.model(cx);
    assert!(staged.update(cx, |m, cx| m.stage_cell(0, 1, "z", cx)));
    let shell = o.shell.clone();
    cx.update_window(o.win, |_, w, cx| {
        shell.update(cx, |s, cx| s.open_connection(0, w, cx))
    })
    .unwrap();
    run_long(cx, &o);
    quit(cx, &o);
    assert_eq!(
        asked(cx, &o).as_deref(),
        Some("Closing Dbento drops them. Closing Dbento cancels the statement running in Query 2.")
    );
    click_id(cx, o.win, "discard-close");
    wait_real(cx, "the window to close", closed);
}

#[gpui_kit::test]
fn disconnect_asks_while_a_statement_runs_on_that_connection(cx: &mut TestAppContext) {
    let o = running_long(cx);
    let m = o.model(cx);
    // Another connection's disconnect leaves this run alone without asking.
    disconnect(cx, &o, "nopg");
    assert!(!present(cx, o.win, "confirm-dialog"));
    assert!(m.read_with(cx, |m, _| m.running()));
    disconnect(cx, &o, "lite");
    assert_eq!(
        asked(cx, &o).as_deref(),
        Some("Disconnecting cancels the statement running in Query 1.")
    );
    click_id(cx, o.win, "discard-keep");
    assert!(m.read_with(cx, |m, _| m.running() && m.events().is_some()));
    disconnect(cx, &o, "lite");
    asked(cx, &o);
    click_id(cx, o.win, "discard-close");
    wait_real(cx, "the disconnect", |cx| {
        m.read_with(cx, |m, _| !m.running() && m.events().is_none())
    });
    assert!(!closed(cx));
}

#[gpui_kit::test]
fn a_workspace_switch_asks_while_a_statement_runs_and_keep_keeps_it_running(
    cx: &mut TestAppContext,
) {
    let o = running_long(cx);
    let m = o.model(cx);
    let switch = |cx: &mut TestAppContext| {
        let (shell, other) = (o.shell.clone(), o.files.path().to_path_buf());
        cx.update_window(o.win, |_, w, cx| {
            shell.update(cx, |s, cx| s.switch_workspace(other, w, cx))
        })
        .unwrap();
        cx.run_until_parked();
    };
    switch(cx);
    assert_eq!(
        asked(cx, &o).as_deref(),
        Some("Switching workspaces cancels the statement running in Query 1.")
    );
    click_id(cx, o.win, "discard-keep");
    assert!(m.read_with(cx, |m, _| m.running()), "Keep cancels nothing");
    assert_eq!(o.shell.read_with(cx, |s, _| s.tabs().len()), 1);
    switch(cx);
    asked(cx, &o);
    click_id(cx, o.win, "discard-close");
    wait_real(cx, "the old session to end", |cx| {
        m.read_with(cx, |m, _| !m.running())
    });
    assert!(!closed(cx));
}
