mod support;
use dbento_app::link::LinkState;
use dbento_app::runner::{Event, SETTINGS_LOST, StatementEnd};
use dbento_app::tab_model::TabModel;
use dbento_core::{CellRef, ExecuteError};
use gpui_kit::{AppContext as _, Entity, TestAppContext};
use support::{LONG, fixture, init_tokio, spec, wait_real};

fn model(cx: &mut TestAppContext, fx: &support::Fx) -> Entity<TabModel> {
    init_tokio(cx);
    cx.new(|_| TabModel::new(spec(fx, "lite")))
}
fn run_to_end(cx: &mut TestAppContext, m: &Entity<TabModel>, sql: &str) {
    m.update(cx, |m, cx| m.run(sql.into(), cx));
    wait_real(cx, "the run", |cx| m.read_with(cx, |m, _| !m.running()));
}
const T: &str = "CREATE TABLE t(x); INSERT INTO t VALUES (1),(2),(3);";

#[gpui_kit::test]
fn a_run_fills_the_store(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = model(cx, &fx);
    run_to_end(cx, &m, "SELECT x FROM t ORDER BY x");
    m.read_with(cx, |m, _| {
        let r = &m.results()[0];
        assert_eq!(
            (r.store.rows(), r.store.cell(2, 0)),
            (3, Some(CellRef::Text("3")))
        );
        assert!(matches!(r.store.end(), Some(StatementEnd::Complete(_))));
        assert!(r.elapsed.is_some());
        assert_eq!(m.read_only(), Some(false));
    });
}
#[gpui_kit::test]
fn skip_and_confirm_answer_the_pending_statement(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = model(cx, &fx);
    m.update(cx, |m, cx| m.run("UPDATE t SET x = 0".into(), cx));
    wait_real(cx, "the question", |cx| {
        m.read_with(cx, |m, _| m.confirm().is_some())
    });
    m.update(cx, |m, cx| m.answer(false, cx));
    wait_real(cx, "the skip", |cx| m.read_with(cx, |m, _| !m.running()));
    m.read_with(cx, |m, _| {
        let r = &m.results()[0];
        assert!(matches!(r.store.end(), Some(StatementEnd::Skipped)));
        assert_eq!(r.sql, "UPDATE t SET x = 0");
    });
    assert_eq!(fx.count("SELECT count(*) FROM t WHERE x = 0"), 0);
    m.update(cx, |m, cx| m.run("UPDATE t SET x = 0".into(), cx));
    wait_real(cx, "the question", |cx| {
        m.read_with(cx, |m, _| m.confirm().is_some())
    });
    m.update(cx, |m, cx| m.answer(true, cx));
    wait_real(cx, "the update", |cx| m.read_with(cx, |m, _| !m.running()));
    assert_eq!(fx.count("SELECT count(*) FROM t WHERE x = 0"), 3);
}
#[gpui_kit::test]
// Cancel at a question ends the run like any other Cancel; the dialog's Skip skips one statement.
fn cancel_while_confirming_skips_the_statement_and_ends_the_script(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = model(cx, &fx);
    m.update(cx, |m, cx| m.run("UPDATE t SET x = 0; SELECT 5".into(), cx));
    wait_real(cx, "the question", |cx| {
        m.read_with(cx, |m, _| m.confirm().is_some())
    });
    m.update(cx, |m, _| m.cancel());
    m.read_with(cx, |m, _| {
        assert!(m.confirm().is_none(), "the question is withdrawn at once")
    });
    // A second Cancel before RunDone finds nothing executing, so it fires nothing either.
    m.update(cx, |m, _| m.cancel());
    m.read_with(cx, |m, _| assert_eq!(m.cancels_fired(), 0));
    wait_real(cx, "the stop", |cx| m.read_with(cx, |m, _| !m.running()));
    m.read_with(cx, |m, _| {
        assert_eq!(m.results().len(), 1, "no later statement ran");
        assert!(matches!(
            m.results()[0].store.end(),
            Some(StatementEnd::Skipped)
        ));
    });
    assert_eq!(fx.count("SELECT count(*) FROM t WHERE x = 0"), 0);
}
#[gpui_kit::test]
fn stale_events_are_dropped(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = model(cx, &fx);
    run_to_end(cx, &m, "SELECT 1");
    m.update(cx, |m, _| {
        m.apply(Event::NeedsConfirm {
            run_id: 0,
            stmt: 0,
            sql: "x".into(),
            kinds: Vec::new(),
            analyze: false,
        });
        m.apply(Event::StatementDone {
            run_id: 0,
            stmt: 0,
            end: StatementEnd::Skipped,
            elapsed: Default::default(),
        });
        assert!(m.confirm().is_none());
        assert!(matches!(
            m.results()[0].store.end(),
            Some(StatementEnd::Complete(_))
        ));
        m.apply(Event::ConnectFailed {
            message: "nope".into(),
        });
        assert_eq!(m.error(), Some("nope"));
    });
}
#[gpui_kit::test]
fn a_disconnected_session_reads_not_connected(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = model(cx, &fx);
    run_to_end(cx, &m, "SELECT 1");
    m.update(cx, |m, _| {
        assert_eq!(m.link(), LinkState::Connected);
        m.apply(Event::Disconnected {
            reason: "gone".into(),
        });
        assert_eq!(m.link(), LinkState::Idle);
        assert_eq!(
            m.read_only(),
            Some(false),
            "edit rules keep the last known mode"
        );
    });
}
#[gpui_kit::test]
fn a_statement_that_fails_to_prepare_keeps_its_text(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = model(cx, &fx);
    run_to_end(cx, &m, "SELECT 1;\n-- broken\nSELEC x");
    m.read_with(cx, |m, _| {
        assert_eq!(m.results().len(), 2);
        assert_eq!(m.results()[1].sql, "-- broken\nSELEC x");
        assert!(matches!(
            m.results()[1].store.end(),
            Some(StatementEnd::Failed(ExecuteError::Database { .. }))
        ));
    });
}
#[gpui_kit::test]
fn a_failure_is_listed_even_before_it_starts(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = model(cx, &fx);
    run_to_end(cx, &m, "SELECT 1; SELEC x");
    m.read_with(cx, |m, _| {
        assert_eq!(m.results().len(), 2);
        assert!(matches!(
            m.results()[1].store.end(),
            Some(StatementEnd::Failed(ExecuteError::Database { .. }))
        ));
    });
}
#[gpui_kit::test]
fn a_second_run_cancels_the_first(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = model(cx, &fx);
    m.update(cx, |m, cx| m.run(LONG.into(), cx));
    wait_real(cx, "the connection", |cx| {
        m.read_with(cx, |m, _| m.read_only().is_some())
    });
    std::thread::sleep(std::time::Duration::from_millis(200));
    run_to_end(cx, &m, "SELECT 7");
    m.read_with(cx, |m, _| {
        assert_eq!(m.results().len(), 1);
        assert_eq!(m.results()[0].store.cell(0, 0), Some(CellRef::Text("7")));
    });
}
#[gpui_kit::test]
// Fails if the old pump survives: it would apply Stopped to the new run when its channel closes.
fn a_stopped_runner_is_replaced_on_the_next_run(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = model(cx, &fx);
    run_to_end(cx, &m, "SELECT 1");
    m.update(cx, |m, _| {
        m.apply(Event::Stopped {
            message: "session stopped".into(),
        })
    });
    m.read_with(cx, |m, _| assert_eq!(m.error(), Some("session stopped")));
    // Keep the new run going until the old runner has surely ended and closed its channel.
    m.update(cx, |m, cx| m.run(LONG.into(), cx));
    let until = std::time::Instant::now() + std::time::Duration::from_millis(300);
    while std::time::Instant::now() < until {
        cx.run_until_parked();
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    m.read_with(cx, |m, _| {
        assert_eq!((m.running(), m.error()), (true, None))
    });
    run_to_end(cx, &m, "SELECT 2");
    m.read_with(cx, |m, _| {
        assert_eq!(m.error(), None);
        assert_eq!(m.results()[0].store.cell(0, 0), Some(CellRef::Text("2")));
    });
}
#[gpui_kit::test]
// SQLite's first step never ends here, so the row must appear before `StatementStarted`.
fn a_long_statement_is_listed_while_it_runs_and_keeps_its_text(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = model(cx, &fx);
    m.update(cx, |m, cx| m.run(LONG.into(), cx));
    wait_real(cx, "the running row", |cx| {
        m.read_with(cx, |m, _| {
            m.results().first().is_some_and(|r| r.sql == LONG)
        })
    });
    m.read_with(cx, |m, _| {
        assert!(m.running() && m.results()[0].store.end().is_none())
    });
    m.update(cx, |m, _| m.cancel());
    m.read_with(cx, |m, _| assert_eq!(m.cancels_fired(), 1));
    wait_real(cx, "the cancel", |cx| m.read_with(cx, |m, _| !m.running()));
    m.read_with(cx, |m, _| {
        let r = &m.results()[0];
        assert!(
            matches!(
                r.store.end(),
                Some(StatementEnd::Failed(ExecuteError::Cancelled))
            ),
            "{:?}",
            r.store.end()
        );
        assert_eq!(r.sql, LONG);
    });
}

#[gpui_kit::test]
fn an_idle_cancel_fires_nothing(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = model(cx, &fx);
    run_to_end(cx, &m, "SELECT 1");
    m.update(cx, |m, _| m.cancel());
    m.read_with(cx, |m, _| assert_eq!(m.cancels_fired(), 0));
    run_to_end(cx, &m, "SELECT 2");
    m.read_with(cx, |m, _| {
        assert_eq!(m.results()[0].store.cell(0, 0), Some(CellRef::Text("2")))
    });
}

#[gpui_kit::test]
// The withdrawn run executes nothing, so a cancel fired by the next Run would hit its statement.
fn a_run_right_after_a_stop_at_a_question_fires_no_cancel(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = model(cx, &fx);
    m.update(cx, |m, cx| m.run("UPDATE t SET x = 0; SELECT 5".into(), cx));
    wait_real(cx, "the question", |cx| {
        m.read_with(cx, |m, _| m.confirm().is_some())
    });
    m.update(cx, |m, cx| {
        m.cancel();
        m.run("SELECT 7".into(), cx);
    });
    m.read_with(cx, |m, _| assert_eq!(m.cancels_fired(), 0));
    wait_real(cx, "the new run", |cx| m.read_with(cx, |m, _| !m.running()));
    m.read_with(cx, |m, _| {
        let r = &m.results()[0];
        assert!(matches!(r.store.end(), Some(StatementEnd::Complete(_))));
        assert_eq!(r.store.cell(0, 0), Some(CellRef::Text("7")));
    });
}

#[gpui_kit::test]
// A withdrawn run leaves Cancel with no target; the next run must give it one again.
fn a_run_after_a_withdrawn_one_can_cancel_its_live_statement(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = model(cx, &fx);
    m.update(cx, |m, cx| m.run("UPDATE t SET x = 0".into(), cx));
    wait_real(cx, "the question", |cx| {
        m.read_with(cx, |m, _| m.confirm().is_some())
    });
    m.update(cx, |m, _| m.cancel());
    wait_real(cx, "the stop", |cx| m.read_with(cx, |m, _| !m.running()));
    m.update(cx, |m, cx| m.run(LONG.into(), cx));
    wait_real(cx, "the running row", |cx| {
        m.read_with(cx, |m, _| {
            m.results().first().is_some_and(|r| r.sql == LONG)
        })
    });
    m.update(cx, |m, _| m.cancel());
    m.read_with(cx, |m, _| assert_eq!(m.cancels_fired(), 1));
    wait_real(cx, "the cancel", |cx| m.read_with(cx, |m, _| !m.running()));
    m.read_with(cx, |m, _| {
        assert!(matches!(
            m.results()[0].store.end(),
            Some(StatementEnd::Failed(ExecuteError::Cancelled))
        ))
    });
}
#[gpui_kit::test]
fn a_run_the_runner_refuses_shows_why_and_ends(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = model(cx, &fx);
    m.update(cx, |m, cx| {
        m.run(LONG.into(), cx);
        let run_id = m.run_id();
        m.apply(Event::SettingsLost { run_id });
        m.apply(Event::RunDone { run_id });
        assert_eq!(m.notice(), Some(SETTINGS_LOST));
        assert!(!m.running());
        assert!(m.results().is_empty(), "no statement is listed");
    });
}
#[gpui_kit::test]
// A tab that drops a session whose settings changed refuses the next run itself, once.
fn a_disconnect_after_a_settings_change_refuses_the_next_run_once(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = model(cx, &fx);
    run_to_end(cx, &m, "SELECT 1");
    m.update(cx, |m, _| m.apply(Event::SettingsChanged));
    drop(m.update(cx, |m, cx| m.disconnect(cx)));
    m.update(cx, |m, cx| m.run("INSERT INTO t VALUES (9)".into(), cx));
    m.read_with(cx, |m, _| {
        assert_eq!(m.notice(), Some(SETTINGS_LOST));
        assert!(!m.running());
        assert_eq!(m.link(), LinkState::Idle, "no runner was started");
    });
    assert_eq!(fx.count("SELECT count(*) FROM t WHERE x = 9"), 0);
    run_to_end(cx, &m, "SELECT 2");
    m.read_with(cx, |m, _| {
        assert_eq!(m.notice(), None);
        assert_eq!(m.results()[0].store.cell(0, 0), Some(CellRef::Text("2")));
    });
}
#[gpui_kit::test]
fn a_stopped_runner_after_a_settings_change_refuses_the_next_run(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = model(cx, &fx);
    run_to_end(cx, &m, "SELECT 1");
    m.update(cx, |m, _| {
        m.apply(Event::SettingsChanged);
        m.apply(Event::Stopped {
            message: "session stopped".into(),
        });
    });
    m.update(cx, |m, cx| m.run("SELECT 2".into(), cx));
    m.read_with(cx, |m, _| {
        assert_eq!((m.notice(), m.running()), (Some(SETTINGS_LOST), false))
    });
    run_to_end(cx, &m, "SELECT 3");
    m.read_with(cx, |m, _| {
        assert_eq!(m.results()[0].store.cell(0, 0), Some(CellRef::Text("3")))
    });
}
#[gpui_kit::test]
fn a_disconnect_without_a_settings_change_runs_on(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = model(cx, &fx);
    run_to_end(cx, &m, "SELECT 1");
    drop(m.update(cx, |m, cx| m.disconnect(cx)));
    run_to_end(cx, &m, "SELECT 2");
    m.read_with(cx, |m, _| {
        assert_eq!(m.notice(), None);
        assert_eq!(m.results()[0].store.cell(0, 0), Some(CellRef::Text("2")));
    });
}
#[gpui_kit::test]
fn a_switched_connection_does_not_inherit_lost_settings(cx: &mut TestAppContext) {
    let extra = "\n[connections.lite2]\nengine = \"sqlite\"\npath = \"data/app.db\"\n";
    let fx = fixture(T, extra);
    let m = model(cx, &fx);
    run_to_end(cx, &m, "SELECT 1");
    m.update(cx, |m, _| m.apply(Event::SettingsChanged));
    drop(m.update(cx, |m, cx| m.disconnect(cx)));
    let ws = fx.ws.clone();
    let switched = m.update(cx, |m, cx| m.switch_connection("lite2", ws, cx));
    drop(switched.expect("lite2 is another connection"));
    run_to_end(cx, &m, "SELECT 2");
    m.read_with(cx, |m, _| {
        assert_eq!(m.notice(), None);
        assert_eq!(m.results()[0].store.cell(0, 0), Some(CellRef::Text("2")));
    });
}
