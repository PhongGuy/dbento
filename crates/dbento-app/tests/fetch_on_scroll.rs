//! Scrolling pulls rows; Cancel stops a parked stream; the status bar shows the session.
use crate::support;
use crate::support::{LONG, MANY, label_of, opened, present, wait_real};
use dbento_app::results_pane::summary;
use dbento_app::runner::{Event, StatementEnd, StopReason};
use gpui_kit::TestAppContext;

fn rows(cx: &mut TestAppContext, o: &support::Opened) -> u64 {
    o.model(cx)
        .read_with(cx, |m, _| m.results().first().map_or(0, |r| r.store.rows()))
}
fn parked(cx: &mut TestAppContext, o: &support::Opened, sql: &str) {
    let m = o.model(cx);
    m.update(cx, |m, _| m.set_max_rows(Some(10_000)));
    o.run(cx, sql);
    wait_real(cx, "the parked stream", |cx| {
        m.read_with(cx, |m, _| m.has_more())
    });
}

#[gpui_kit::test]
fn scrolling_to_the_end_pulls_one_more_batch(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    parked(cx, &o, MANY);
    assert_eq!(rows(cx, &o), 2048);
    let table = o.shell.read_with(cx, |s, cx| {
        s.tab()
            .unwrap()
            .read(cx)
            .results_pane()
            .read(cx)
            .table()
            .clone()
    });
    table.update(cx, |t, cx| t.scroll_to_row(2047, cx));
    // Each poll renders a frame, so the kit sees the new scroll position and asks for more.
    wait_real(cx, "one more batch", |cx| {
        present(cx, o.win, "result-grid") && rows(cx, &o) >= 3072
    });
    for _ in 0..3 {
        std::thread::sleep(std::time::Duration::from_millis(100));
        assert!(present(cx, o.win, "result-grid"));
    }
    assert_eq!(rows(cx, &o), 3072, "row 2047 is no longer near the end");
}

#[gpui_kit::test]
fn a_parked_statement_says_it_is_paused(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    parked(cx, &o, MANY);
    assert_eq!(
        label_of(cx, o.win, ("statement-result", 0usize)).as_deref(),
        Some("first 2,048 rows · more available")
    );
}

#[gpui_kit::test]
fn cancel_while_parked_stops_and_keeps_the_rows(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    parked(cx, &o, MANY);
    let m = o.model(cx);
    m.update(cx, |m, _| m.cancel());
    wait_real(cx, "the stop", |cx| {
        m.read_with(cx, |m, _| m.paused().is_none())
    });
    m.read_with(cx, |m, _| {
        let end = m.results()[0].store.end();
        assert!(
            matches!(
                end,
                Some(StatementEnd::Stopped {
                    rows: 2048,
                    reason: StopReason::User
                })
            ),
            "{end:?}"
        );
    });
}

#[gpui_kit::test]
fn a_new_run_over_a_parked_stream_runs(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    parked(cx, &o, MANY);
    o.run(cx, "SELECT 7");
    o.idle(cx);
    assert_eq!(label_of(cx, o.win, "cell-0-0").as_deref(), Some("7"));
}

#[gpui_kit::test]
fn the_status_bar_shows_the_drain_and_the_notice(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let m = o.model(cx);
    // Opening the tab connects in the background, and any event clears the drain flag, so
    // the hand-fed events start only once `Connected` has landed.
    wait_real(cx, "the connection", |cx| {
        m.read_with(cx, |m, _| m.read_only().is_some())
    });
    m.update(cx, |m, cx| {
        m.apply(Event::Settling { run_id: 999 });
        cx.notify();
    });
    assert_eq!(
        label_of(cx, o.win, "status-session").as_deref(),
        Some("discarding remaining rows")
    );
    m.update(cx, |m, cx| {
        m.apply(Event::RunDone { run_id: 999 });
        cx.notify();
    });
    assert!(!present(cx, o.win, "status-session"));
    m.update(cx, |m, cx| {
        m.apply(Event::SettleFailed {
            message: "boom".into(),
        });
        cx.notify();
    });
    assert_eq!(
        label_of(cx, o.win, "status-session").as_deref(),
        Some("boom")
    );
}

#[gpui_kit::test]
fn after_stopped_no_statement_still_says_running(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let m = o.model(cx);
    o.run(cx, LONG);
    wait_real(cx, "the running row", |cx| {
        m.read_with(cx, |m, _| !m.results().is_empty())
    });
    m.update(cx, |m, cx| {
        m.apply(Event::Stopped {
            message: "session stopped".into(),
        });
        cx.notify();
    });
    m.read_with(cx, |m, _| {
        assert_eq!(summary(&m.results()[0]), "session stopped")
    });
}

#[gpui_kit::test]
fn truncated_at_shows_the_runs_cap(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    o.model(cx).update(cx, |m, _| m.set_max_rows(Some(1500)));
    o.run(cx, MANY);
    o.idle(cx);
    // A read cut by its cap parks there; Stop ends it truncated with its rows.
    let m = o.model(cx);
    m.update(cx, |m, _| m.cancel());
    wait_real(cx, "the stop", |cx| {
        m.read_with(cx, |m, _| m.results()[0].store.end().is_some())
    });
    assert_eq!(
        label_of(cx, o.win, "status-truncated").as_deref(),
        Some("truncated at 1,500 rows")
    );
}

// Fast to row 3,100, then one 30 s pause (the uncorrelated sleep runs once, as an InitPlan).
// Postgres batches end where the socket runs dry, so their sizes vary: the test pulls until a
// Pull gets no batch, which means the read-ahead behind the last batch is blocked on the pause.
const SLOW_TAIL: &str = "SELECT g, CASE WHEN g > 3100 THEN (SELECT 1 FROM pg_sleep(30)) END FROM generate_series(1, 5000) g";

type Model = gpui_kit::Entity<dbento_app::tab_model::TabModel>;

/// The app's tokio runtime; a test holds it entered so the database drops inside it.
fn runtime(cx: &mut TestAppContext) -> tokio::runtime::Handle {
    support::init_tokio(cx);
    cx.update(|cx| dbento_app::tokio_bridge::handle(cx))
}

/// A tab model over a fresh Postgres database, with `setup` run on it first.
fn pg_model(
    cx: &mut TestAppContext,
    rt: &tokio::runtime::Handle,
    extra: &str,
    setup: &str,
) -> (Model, dbento_testkit::PgDb, tempfile::TempDir) {
    use gpui_kit::AppContext as _;
    let (db, dir, spec) = rt.block_on(support::pg_spec(extra));
    if !setup.is_empty() {
        rt.block_on(async { db.admin().await.batch_execute(setup).await.unwrap() });
    }
    (
        cx.new(|_| dbento_app::tab_model::TabModel::new(spec)),
        db,
        dir,
    )
}

fn first_rows(cx: &mut TestAppContext, m: &Model) -> u64 {
    m.read_with(cx, |m, _| m.results().first().map_or(0, |r| r.store.rows()))
}

/// Runs SLOW_TAIL and pulls until a Pull gets no batch; returns the rows sent by then.
fn blocked_on_the_slow_tail(cx: &mut TestAppContext, m: &Model) -> u64 {
    use std::time::{Duration, Instant};
    m.update(cx, |m, cx| {
        m.set_max_rows(Some(10_000));
        m.run(SLOW_TAIL.into(), cx);
    });
    wait_real(cx, "the park", |cx| m.read_with(cx, |m, _| m.has_more()));
    loop {
        let before = first_rows(cx, m);
        assert!(before <= 3100, "a slow row arrived: {before}");
        m.update(cx, |m, _| m.pull());
        let until = Instant::now() + Duration::from_millis(700);
        while first_rows(cx, m) == before && Instant::now() < until {
            cx.run_until_parked();
            std::thread::sleep(Duration::from_millis(5));
        }
        if first_rows(cx, m) == before {
            return before;
        }
        wait_real(cx, "the park", |cx| m.read_with(cx, |m, _| m.has_more()));
    }
}

/// Waits at most `limit` for the parked result to end; returns how long it took.
fn until_unparked(
    cx: &mut TestAppContext,
    m: &Model,
    limit: std::time::Duration,
) -> std::time::Duration {
    let asked = std::time::Instant::now();
    while m.read_with(cx, |m, _| m.paused().is_some()) && asked.elapsed() < limit {
        cx.run_until_parked();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    asked.elapsed()
}

fn assert_stopped_at(cx: &mut TestAppContext, m: &Model, sent: u64) {
    m.read_with(cx, |m, _| {
        let end = m.results()[0].store.end();
        assert!(
            matches!(
                end,
                Some(StatementEnd::Stopped {
                    rows,
                    reason: StopReason::User
                }) if *rows == sent
            ),
            "{end:?}, {sent} sent"
        );
    });
}

/// The session still runs the next statement: no cancel was left armed for it.
fn next_run_answers(cx: &mut TestAppContext, m: &Model) {
    m.update(cx, |m, cx| m.run("SELECT 'ok'".into(), cx));
    wait_real(cx, "the next run", |cx| {
        m.read_with(cx, |m, _| {
            !m.running() && m.results().first().is_some_and(|r| r.store.end().is_some())
        })
    });
    m.read_with(cx, |m, _| {
        let r = &m.results()[0];
        assert_eq!(
            r.store.cell(0, 0),
            Some(dbento_core::CellRef::Text("ok")),
            "{:?}",
            r.store.end()
        );
    });
}

// A read-only Stop closes the session anyway, so its Cancel frees the blocked read at once.
#[gpui_kit::test]
fn cancel_reaches_a_paused_results_blocked_read_ahead(cx: &mut TestAppContext) {
    let rt = runtime(cx);
    let _enter = rt.enter();
    let (m, _db, _dir) = pg_model(cx, &rt, "read_only = true", "");
    let sent = blocked_on_the_slow_tail(cx, &m);
    m.update(cx, |m, _| m.cancel());
    // Bounded well under the pause, so a Stop that waits for the tail fails promptly.
    let took = until_unparked(cx, &m, std::time::Duration::from_secs(2));
    assert!(took < std::time::Duration::from_secs(1), "{took:?}");
    assert_stopped_at(cx, &m, sent);
    next_run_answers(cx, &m);
}

// Each row inserts one, and 50 MB of rows outrun the socket, so the backend is mid-write at the park.
const WRITING_FN: &str = "CREATE TABLE hits (g int);
CREATE FUNCTION bump(g int) RETURNS text LANGUAGE plpgsql AS $$
BEGIN INSERT INTO hits VALUES (g); RETURN repeat('x', 1000); END $$;";
const WRITING: &str = "SELECT bump(g) FROM generate_series(1, 50000) g";

#[gpui_kit::test]
fn a_read_write_cancel_while_parked_keeps_the_writes(cx: &mut TestAppContext) {
    let rt = runtime(cx);
    let _enter = rt.enter();
    let (m, db, _dir) = pg_model(cx, &rt, "", WRITING_FN);
    m.update(cx, |m, cx| {
        m.set_max_rows(Some(1_000_000));
        m.run(WRITING.into(), cx);
    });
    wait_real(cx, "the park", |cx| m.read_with(cx, |m, _| m.has_more()));
    m.update(cx, |m, _| m.cancel());
    wait_real(cx, "the run's end", |cx| {
        m.read_with(cx, |m, _| {
            !m.running() && m.paused().is_none() && !m.settling()
        })
    });
    let count = rt.block_on(db.scalar("SELECT count(*) FROM hits"));
    assert_eq!(count.as_deref(), Some("50000"));
    m.read_with(cx, |m, _| assert_eq!(m.notice(), None));
}

#[gpui_kit::test]
fn a_second_read_write_cancel_frees_a_blocked_drain(cx: &mut TestAppContext) {
    use std::time::Duration;
    let rt = runtime(cx);
    let _enter = rt.enter();
    let (m, _db, _dir) = pg_model(cx, &rt, "", "");
    let sent = blocked_on_the_slow_tail(cx, &m);
    m.update(cx, |m, _| m.cancel());
    // The first Cancel only stops, so the drain waits on the slow tail for its writes.
    let took = until_unparked(cx, &m, Duration::from_millis(500));
    assert!(
        took >= Duration::from_millis(500),
        "the first cancel fired: {took:?}"
    );
    m.update(cx, |m, _| m.cancel());
    let took = until_unparked(cx, &m, Duration::from_secs(2));
    assert!(took < Duration::from_secs(1), "{took:?}");
    assert_stopped_at(cx, &m, sent);
    next_run_answers(cx, &m);
}

#[gpui_kit::test]
fn a_read_parked_at_its_cap_is_not_running_and_stop_ends_it_truncated(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let m = o.model(cx);
    m.update(cx, |m, _| m.set_max_rows(Some(1_000)));
    o.run(cx, MANY);
    wait_real(cx, "the park at the cap", |cx| {
        m.read_with(cx, |m, _| m.parked_at_cap().is_some())
    });
    m.read_with(cx, |m, _| {
        assert_eq!(m.parked_at_cap(), Some(0));
        assert!(!m.running());
        assert!(!m.has_more(), "scrolling sends no Pull at the cap");
        assert!(m.results()[0].store.end().is_none());
        assert!(m.results()[0].elapsed.is_some());
    });
    m.update(cx, |m, _| m.cancel());
    wait_real(cx, "the stop", |cx| {
        m.read_with(cx, |m, _| m.results()[0].store.end().is_some())
    });
    m.read_with(cx, |m, _| {
        let end = m.results()[0].store.end();
        assert!(
            matches!(end, Some(StatementEnd::Truncated(o)) if o.rows == 1000),
            "{end:?}"
        );
        assert_eq!(m.parked_at_cap(), None);
    });
}
