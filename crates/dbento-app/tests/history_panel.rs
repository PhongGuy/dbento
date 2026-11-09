use crate::support;
use crate::support::{label_of, opened, present, wait_real};
use dbento_app::app::{AppPaths, start};
use dbento_app::history_panel::{
    TIME_REFRESH, one_line, outcome_label, outcome_mark, relative_time,
};
use dbento_app::ui::Icons;
use dbento_core::{Engine, History, HistoryEntry, HistoryOutcome};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AnyWindowHandle, AppContext as _, Entity, TestAppContext};
use gpui_kit::{hsla, px};
use std::path::Path;
use std::time::{Duration, SystemTime};

const OK: HistoryOutcome = HistoryOutcome::Ok {
    rows: 3,
    truncated: false,
    rows_affected: None,
};

fn seed(files: &Path, ws: &Path) {
    let mut h = History::open(&AppPaths::in_dir(files).history).unwrap();
    let e = |ws: &Path, conn: &str, sql: &str, o: HistoryOutcome| {
        HistoryEntry::new(
            SystemTime::now(),
            ws,
            conn,
            sql,
            Duration::from_micros(4_321),
            o,
        )
    };
    h.append(&e(ws, "lite", "SELECT 1", OK)).unwrap();
    h.append(&e(
        ws,
        "nopg",
        "SELECT 2 FROM pg",
        HistoryOutcome::Cancelled,
    ))
    .unwrap();
    h.append(&e(
        ws,
        "lite",
        "\n  DELETE FROM t\nWHERE x = 1",
        HistoryOutcome::Failed {
            message: "no such table: t\nmore".into(),
        },
    ))
    .unwrap();
    h.append(&e(Path::new("/elsewhere"), "lite", "SELECT elsewhere", OK))
        .unwrap();
}
type Started = (
    support::Fx,
    tempfile::TempDir,
    AnyWindowHandle,
    Entity<dbento_app::shell::Shell>,
);
fn started(cx: &mut TestAppContext) -> Started {
    started_on(cx, "")
}
// `extra` lands inside `[connections.lite]`.
fn started_on(cx: &mut TestAppContext, extra: &str) -> Started {
    support::init(cx);
    support::init_tokio(cx);
    let (fx, files) = (support::fixture("", extra), tempfile::tempdir().unwrap());
    seed(files.path(), fx.ws.dir());
    let (win, shell) = cx
        .update(|cx| {
            start(
                support::services(files.path()),
                Some(fx.dir.path().into()),
                cx,
            )
        })
        .unwrap();
    assert!(present(cx, win, "connection-list"));
    cx.update_window(win, |_, w, cx| w.press("ctrl-shift-h", cx))
        .unwrap();
    assert!(present(cx, win, "history-panel"));
    (fx, files, win, shell)
}
fn rows(cx: &mut TestAppContext, win: AnyWindowHandle) -> Vec<String> {
    (0usize..)
        .map_while(|i| label_of(cx, win, ("history-row", i)))
        .collect()
}
fn wait_rows(cx: &mut TestAppContext, win: AnyWindowHandle, want: &[&str]) {
    wait_real(cx, "the rows", |cx| rows(cx, win) == want);
}

#[gpui_kit::test]
fn the_panel_lists_this_workspaces_runs_newest_first(cx: &mut TestAppContext) {
    let (_fx, _files, win, _) = started(cx);
    wait_rows(
        cx,
        win,
        &["DELETE FROM t WHERE x = 1", "SELECT 2 FROM pg", "SELECT 1"],
    );
    assert_eq!(
        label_of(cx, win, ("history-detail", 0usize)).as_deref(),
        Some("lite · failed: no such table: t · 4 ms")
    );
    assert_eq!(
        label_of(cx, win, ("history-detail", 1usize)).as_deref(),
        Some("nopg · cancelled · 4 ms")
    );
    assert_eq!(
        label_of(cx, win, ("history-detail", 2usize)).as_deref(),
        Some("lite · 3 rows · 4 ms")
    );
}

#[gpui_kit::test]
fn search_narrows_case_insensitively(cx: &mut TestAppContext) {
    let (_fx, _files, win, shell) = started(cx);
    let search = shell.read_with(cx, |s, cx| {
        s.history_panel().unwrap().read(cx).search_state().clone()
    });
    cx.update_window(win, |_, w, cx| {
        search.update(cx, |s, cx| s.focus(w, cx));
        w.input("from PG", cx);
    })
    .unwrap();
    wait_rows(cx, win, &["SELECT 2 FROM pg"]);
}

#[test]
fn one_line_joins_the_statement_and_drops_comment_lines() {
    assert_eq!(
        one_line(Engine::Sqlite, "\n  DELETE FROM t\nWHERE x = 1"),
        "DELETE FROM t WHERE x = 1"
    );
    assert_eq!(
        one_line(Engine::Sqlite, "-- note\nSELECT *\n  FROM a\n\n"),
        "SELECT * FROM a"
    );
    assert_eq!(one_line(Engine::Sqlite, "SELECT 'a  b'"), "SELECT 'a  b'");
}

#[test]
fn a_dash_dash_line_inside_a_literal_stays_in_the_label() {
    assert_eq!(
        one_line(Engine::Sqlite, "SELECT 'a\n-- b\nc'\n-- note\nFROM t"),
        "SELECT 'a -- b c' FROM t"
    );
    assert_eq!(
        one_line(Engine::Postgres, "SELECT $$x\n  -- y\n$$, \"q\n--r\""),
        "SELECT $$x -- y $$, \"q --r\""
    );
    assert_eq!(
        one_line(Engine::Postgres, "SELECT /* a\n-- b */ 1"),
        "SELECT /* a -- b */ 1",
        "a line inside a block comment is part of that comment"
    );
    assert_eq!(
        one_line(Engine::Sqlite, "SELECT 'open\n-- dropped"),
        "SELECT 'open",
        "text that does not lex falls back to dropping every -- line"
    );
}

#[test]
fn a_comment_only_statement_shows_its_first_line() {
    assert_eq!(
        one_line(Engine::Sqlite, "\n  -- just a note  \n-- another"),
        "-- just a note"
    );
}

#[gpui_kit::test]
fn each_entry_carries_its_connections_tag_dot(cx: &mut TestAppContext) {
    let (_fx, _files, win, _) = started_on(cx, "color = \"red\"");
    wait_rows(
        cx,
        win,
        &["DELETE FROM t WHERE x = 1", "SELECT 2 FROM pg", "SELECT 1"],
    );
    assert_eq!(
        label_of(cx, win, ("history-dot", 0usize)).as_deref(),
        Some("red")
    );
    assert!(
        !present(cx, win, ("history-dot", 1usize)),
        "nopg is untagged"
    );
    assert_eq!(
        label_of(cx, win, ("history-dot", 2usize)).as_deref(),
        Some("red")
    );
}

#[gpui_kit::test]
fn the_connection_filter_is_a_dropdown_of_every_connection(cx: &mut TestAppContext) {
    let (_fx, _files, win, _) = started(cx);
    let open = |cx: &mut TestAppContext| {
        cx.update_window(win, |_, w, cx| w.click("history-connection", cx))
            .unwrap();
        cx.run_until_parked();
    };
    let pick = |cx: &mut TestAppContext, n: usize| {
        cx.update_window(win, |_, w, cx| w.click(("history-filter", n), cx))
            .unwrap();
        cx.run_until_parked();
    };
    assert_eq!(
        label_of(cx, win, "history-connection").as_deref(),
        Some("all connections")
    );
    open(cx);
    assert_eq!(
        label_of(cx, win, ("history-filter", 0usize)).as_deref(),
        Some("all connections")
    );
    assert_eq!(
        label_of(cx, win, ("history-filter", 1usize)).as_deref(),
        Some("lite")
    );
    assert_eq!(
        label_of(cx, win, ("history-filter", 2usize)).as_deref(),
        Some("nopg")
    );
    pick(cx, 2);
    assert_eq!(
        label_of(cx, win, "history-connection").as_deref(),
        Some("nopg")
    );
    wait_rows(cx, win, &["SELECT 2 FROM pg"]);
    open(cx);
    pick(cx, 0);
    wait_rows(
        cx,
        win,
        &["DELETE FROM t WHERE x = 1", "SELECT 2 FROM pg", "SELECT 1"],
    );
}

#[gpui_kit::test]
fn a_row_opens_its_sql_in_a_new_tab_and_never_runs_it(cx: &mut TestAppContext) {
    let (_fx, _files, win, shell) = started(cx);
    wait_rows(
        cx,
        win,
        &["DELETE FROM t WHERE x = 1", "SELECT 2 FROM pg", "SELECT 1"],
    );
    cx.update_window(win, |_, w, cx| w.click(("history-row", 1usize), cx))
        .unwrap();
    cx.run_until_parked();
    let (n, conn, sql, edited, run_id) = shell.read_with(cx, |s, cx| {
        let t = s.tab().unwrap().read(cx);
        let m = t.model().read(cx);
        (
            s.tabs().len(),
            m.spec().connection.clone(),
            t.sql(cx),
            t.edited(cx),
            m.run_id(),
        )
    });
    assert_eq!(
        (n, conn.as_str(), sql.as_str(), edited, run_id),
        (1, "nopg", "SELECT 2 FROM pg", false, 0)
    );
}

#[gpui_kit::test]
fn a_new_run_appears_in_the_open_panel_and_the_toggle_hides_it(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    assert!(
        !present(cx, o.win, "history-panel"),
        "the panel starts closed"
    );
    cx.update_window(o.win, |_, w, cx| w.click("history-toggle", cx))
        .unwrap();
    assert!(present(cx, o.win, "history-empty"));
    o.run(cx, "SELECT 99");
    o.idle(cx);
    wait_rows(cx, o.win, &["SELECT 99"]);
    cx.update_window(o.win, |_, w, cx| w.click("history-toggle", cx))
        .unwrap();
    assert!(!present(cx, o.win, "history-panel"));
}

#[test]
fn outcome_labels_read_as_the_results_pane_does() {
    let ok = |rows, truncated, rows_affected| HistoryOutcome::Ok {
        rows,
        truncated,
        rows_affected,
    };
    assert_eq!(outcome_label(&ok(1, false, None)), "1 row");
    assert_eq!(
        outcome_label(&ok(1_000, true, None)),
        "1,000 rows (truncated)"
    );
    assert_eq!(outcome_label(&ok(0, false, Some(2))), "2 rows affected");
    assert_eq!(
        outcome_label(&HistoryOutcome::Failed {
            message: "boom\nmore".into()
        }),
        "failed: boom"
    );
    assert_eq!(outcome_label(&HistoryOutcome::Cancelled), "cancelled");
    assert_eq!(outcome_label(&HistoryOutcome::Skipped), "skipped");
}

// The dock takes its width from the tab area, never from the explorer beside it.
#[gpui_kit::test]
fn opening_the_panel_keeps_the_sidebar_width(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let width = |cx: &mut TestAppContext, id: &'static str| {
        cx.run_until_parked();
        cx.update_window(o.win, |_, w, cx| {
            w.render_frame(cx);
            w.render_frame(cx);
            w.find(id).bounds().size.width
        })
        .unwrap()
    };
    let before = width(cx, "sidebar");
    cx.update_window(o.win, |_, w, cx| w.click("history-toggle", cx))
        .unwrap();
    for _ in 0..3 {
        assert_eq!(width(cx, "sidebar"), before);
    }
    assert_eq!(width(cx, "history-panel"), gpui_kit::px(320.));
}

fn reads(cx: &mut TestAppContext, shell: &Entity<dbento_app::shell::Shell>) -> u64 {
    shell.read_with(cx, |s, cx| s.history_panel().unwrap().read(cx).reads())
}

// Each write notifies the log; a burst of them must not start a read apiece.
fn notify_log(cx: &mut TestAppContext, shell: &Entity<dbento_app::shell::Shell>, times: usize) {
    let log = shell.read_with(cx, |s, _| s.history_log().clone());
    for _ in 0..times {
        log.update(cx, |_, cx| cx.notify());
    }
}

#[gpui_kit::test]
fn a_burst_of_writes_rereads_at_most_once_more(cx: &mut TestAppContext) {
    let (_fx, _files, win, shell) = started(cx);
    wait_rows(
        cx,
        win,
        &["DELETE FROM t WHERE x = 1", "SELECT 2 FROM pg", "SELECT 1"],
    );
    cx.run_until_parked();
    let before = reads(cx, &shell);
    notify_log(cx, &shell, 50);
    cx.run_until_parked();
    assert_eq!(
        reads(cx, &shell),
        before + 2,
        "the read in flight, then one more"
    );
}

#[gpui_kit::test]
fn a_hidden_panel_reads_nothing_until_shown(cx: &mut TestAppContext) {
    let (_fx, _files, win, shell) = started(cx);
    wait_rows(
        cx,
        win,
        &["DELETE FROM t WHERE x = 1", "SELECT 2 FROM pg", "SELECT 1"],
    );
    cx.run_until_parked();
    let before = reads(cx, &shell);
    cx.update_window(win, |_, w, cx| w.press("ctrl-shift-h", cx))
        .unwrap();
    notify_log(cx, &shell, 5);
    cx.run_until_parked();
    assert_eq!(reads(cx, &shell), before);
    cx.update_window(win, |_, w, cx| w.press("ctrl-shift-h", cx))
        .unwrap();
    cx.run_until_parked();
    assert_eq!(reads(cx, &shell), before + 1);
}

#[gpui_kit::test]
fn a_write_that_writes_nothing_does_not_notify(cx: &mut TestAppContext) {
    use dbento_app::history::HistoryLog;
    let dir = tempfile::tempdir().unwrap();
    // A directory cannot be opened as the history file, so every write fails.
    let log = cx.new(|_| HistoryLog::new(dir.path().into()));
    let notified = std::rc::Rc::new(std::cell::Cell::new(0));
    let seen = notified.clone();
    cx.update(|cx| {
        cx.observe(&log, move |_, _| seen.set(seen.get() + 1))
            .detach()
    });
    let entry = HistoryEntry::new(
        SystemTime::now(),
        dir.path(),
        "lite",
        "SELECT 1",
        Duration::ZERO,
        OK,
    );
    log.update(cx, |l, cx| l.append(vec![entry], cx));
    cx.run_until_parked();
    assert_eq!(notified.get(), 0);
}

#[test]
fn relative_times_read_coarsely() {
    let now = SystemTime::UNIX_EPOCH + Duration::from_secs(10_000_000);
    let ago = |s: u64| relative_time(now - Duration::from_secs(s), now);
    assert_eq!(ago(0), "just now");
    assert_eq!(ago(59), "just now");
    assert_eq!(ago(60), "1 min ago");
    assert_eq!(ago(3_599), "59 min ago");
    assert_eq!(ago(3_600), "1 h ago");
    assert_eq!(ago(86_399), "23 h ago");
    assert_eq!(ago(86_400), "1 d ago");
    assert_eq!(ago(40 * 86_400), "40 d ago");
    assert_eq!(
        relative_time(now + Duration::from_secs(30), now),
        "just now",
        "a clock that stepped back reads as now"
    );
}

#[test]
fn ok_runs_get_a_success_check_failures_a_danger_cross_and_the_rest_a_dim_cross() {
    let (success, danger, dim) = (
        hsla(0.3, 1., 0.5, 1.),
        hsla(0., 1., 0.5, 1.),
        hsla(0., 0., 0.5, 1.),
    );
    let mark = |o: &HistoryOutcome| outcome_mark(o, success, danger, dim);
    assert_eq!(mark(&OK), (Icons::CircleCheck, success));
    let failed = HistoryOutcome::Failed {
        message: "no such table".into(),
    };
    assert_eq!(mark(&failed), (Icons::CircleX, danger));
    assert_eq!(mark(&HistoryOutcome::Cancelled), (Icons::CircleX, dim));
}

#[gpui_kit::test]
fn the_search_is_28_px_and_rows_stay_40_px(cx: &mut TestAppContext) {
    let (_fx, _files, win, _) = started(cx);
    wait_rows(
        cx,
        win,
        &["DELETE FROM t WHERE x = 1", "SELECT 2 FROM pg", "SELECT 1"],
    );
    let (search, row) = cx
        .update_window(win, |_, w, cx| {
            w.render_frame(cx);
            (
                w.find("history-search").bounds().size.height,
                w.find(("history-row", 0usize)).bounds().size.height,
            )
        })
        .unwrap();
    assert!(search >= px(34.), "{search:?}");
    assert_eq!(row, px(40.));
}

// Counts the panel's notifications, which are what redraw its relative times.
fn panel_notes(
    cx: &mut TestAppContext,
    shell: &Entity<dbento_app::shell::Shell>,
) -> std::rc::Rc<std::cell::Cell<u32>> {
    let panel = shell.read_with(cx, |s, _| s.history_panel().unwrap().clone());
    let notes = std::rc::Rc::new(std::cell::Cell::new(0));
    let seen = notes.clone();
    cx.update(|cx| {
        cx.observe(&panel, move |_, _| seen.set(seen.get() + 1))
            .detach()
    });
    notes
}

#[gpui_kit::test]
fn an_open_panel_redraws_its_relative_times_periodically(cx: &mut TestAppContext) {
    let (_fx, _files, win, shell) = started(cx);
    wait_rows(
        cx,
        win,
        &["DELETE FROM t WHERE x = 1", "SELECT 2 FROM pg", "SELECT 1"],
    );
    cx.run_until_parked();
    let notes = panel_notes(cx, &shell);
    cx.executor().advance_clock(TIME_REFRESH);
    cx.run_until_parked();
    assert_eq!(notes.get(), 1, "one redraw per refresh period");
    cx.executor().advance_clock(TIME_REFRESH);
    cx.run_until_parked();
    assert_eq!(notes.get(), 2);
}

#[gpui_kit::test]
fn a_hidden_panel_runs_no_refresh_timer(cx: &mut TestAppContext) {
    let (_fx, _files, win, shell) = started(cx);
    wait_rows(
        cx,
        win,
        &["DELETE FROM t WHERE x = 1", "SELECT 2 FROM pg", "SELECT 1"],
    );
    cx.update_window(win, |_, w, cx| w.press("ctrl-shift-h", cx))
        .unwrap();
    cx.run_until_parked();
    let notes = panel_notes(cx, &shell);
    cx.executor().advance_clock(TIME_REFRESH * 4);
    cx.run_until_parked();
    assert_eq!(notes.get(), 0);
    // Showing it again restarts the refresh.
    cx.update_window(win, |_, w, cx| w.press("ctrl-shift-h", cx))
        .unwrap();
    cx.run_until_parked();
    let after_show = notes.get();
    cx.executor().advance_clock(TIME_REFRESH);
    cx.run_until_parked();
    assert_eq!(notes.get(), after_show + 1);
}
