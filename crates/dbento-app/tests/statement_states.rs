//! How skipped, user-stopped and failed statements read in the results pane and status bar.
use crate::support::{LONG, Opened, click_id, init, label_of, opened, present, wait_real};
use dbento_app::plan_view::PlanView;
use dbento_app::results::ResultStore;
use dbento_app::results_pane::{ResultMark, displayed, result_mark, summary};
use dbento_app::runner::{StatementEnd, StopReason};
use dbento_app::status_bar::{StatusMark, status_mark, status_subject};
use dbento_app::tab_model::{PlanResult, StatementResult};
use dbento_core::{ColumnInfo, ExecuteError, ExplainMode};
use gpui_kit::TestAppContext;
use std::sync::Arc;

const T: &str = "CREATE TABLE t(x); INSERT INTO t VALUES (1),(2),(3);";

fn marks(cx: &mut TestAppContext, o: &Opened) -> Vec<ResultMark> {
    o.model(cx).read_with(cx, |m, _| {
        let asking = m.confirm().map(|p| p.stmt);
        m.results()
            .iter()
            .map(|r| result_mark(r, asking, m.more_rows(r.stmt)))
            .collect()
    })
}

fn ended(stmt: usize, sql: &str, end: Option<StatementEnd>) -> StatementResult {
    let mut r = StatementResult {
        stmt,
        sql: sql.into(),
        engine: None,
        epoch: 0,
        store: ResultStore::new(Arc::from(Vec::<ColumnInfo>::new())),
        elapsed: None,
        source: None,
        plan: None,
    };
    if let Some(end) = end {
        r.store.finish(end);
    }
    r
}

/// Run All up to the UPDATE's question, then the dialog's Cancel.
fn cancelled_at_question(cx: &mut TestAppContext) -> Opened {
    let o = opened(cx, T, "", 0);
    o.run(cx, "SELECT x FROM t; UPDATE t SET x = 0");
    wait_real(cx, "the dialog", |cx| present(cx, o.win, "confirm-dialog"));
    o.model(cx).update(cx, |m, cx| m.answer(false, cx));
    o.idle(cx);
    o
}

#[gpui_kit::test]
fn a_skipped_statement_says_skipped_on_its_tab_and_body(cx: &mut TestAppContext) {
    let o = cancelled_at_question(cx);
    assert_eq!(marks(cx, &o), [ResultMark::Rows(3), ResultMark::Skipped]);
    click_id(cx, o.win, ("statement-result", 1usize));
    assert_eq!(
        label_of(cx, o.win, "result-skipped").as_deref(),
        Some("Skipped")
    );
    assert!(
        !present(cx, o.win, "footer-rows"),
        "a skipped statement has no rows"
    );
}

#[gpui_kit::test]
fn a_skipped_statement_is_the_status_subject_without_a_check(cx: &mut TestAppContext) {
    let o = cancelled_at_question(cx);
    o.model(cx).read_with(cx, |m, _| {
        let results = m.results();
        let ix = status_subject(results, Some(0)).unwrap();
        assert_eq!(ix, 1);
        assert_eq!(
            status_mark(results[ix].store.end(), false),
            StatusMark::Neutral
        );
    });
}

#[test]
fn a_skipped_explain_body_says_skipped() {
    let mut r = ended(0, "UPDATE t SET x = 0", Some(StatementEnd::Skipped));
    r.plan = Some(PlanResult::new(ExplainMode::Analyze, false));
    assert_eq!(
        result_mark(&r, None, false),
        ResultMark::Skipped,
        "the Plan tab shows its mark"
    );
    assert_eq!(
        dbento_app::result_body::result_body(&[r], None),
        dbento_app::result_body::ResultBody::Skipped
    );
}

#[gpui_kit::test]
fn a_cancel_the_user_asked_for_ends_stopped_not_failed(cx: &mut TestAppContext) {
    let o = opened(cx, T, "", 0);
    let m = o.model(cx);
    o.run(cx, LONG);
    wait_real(cx, "the running row", |cx| {
        m.read_with(cx, |m, _| {
            m.results().first().is_some_and(|r| r.sql == LONG)
        })
    });
    m.update(cx, |m, _| m.cancel());
    o.idle(cx);
    m.read_with(cx, |m, _| {
        let r = &m.results()[0];
        assert!(
            matches!(
                r.store.end(),
                Some(StatementEnd::Stopped {
                    reason: StopReason::User,
                    ..
                })
            ),
            "{:?}",
            r.store.end()
        );
        assert_eq!(summary(r), "0 rows (stopped)");
        assert_eq!(status_mark(r.store.end(), false), StatusMark::Neutral);
    });
    assert!(!present(cx, o.win, "result-error"), "no red Query failed");
    assert_eq!(
        label_of(cx, o.win, "result-stopped").as_deref(),
        Some("Stopped")
    );
}

#[test]
fn a_statement_timeout_stays_a_failure() {
    let r = ended(
        0,
        "SELECT 1",
        Some(StatementEnd::Failed(ExecuteError::Timeout)),
    );
    assert_eq!(result_mark(&r, None, false), ResultMark::Failed);
    assert_eq!(status_mark(r.store.end(), false), StatusMark::Failed);
    let done = ended(0, "SELECT 1", Some(StatementEnd::Skipped));
    assert_eq!(status_mark(done.store.end(), true), StatusMark::Running);
    assert_eq!(status_mark(None, false), StatusMark::Done);
}

#[test]
fn without_a_pick_the_first_failed_statement_is_displayed() {
    let ok = ended(0, "SELECT 1", None);
    let bad = ended(
        1,
        "SELECT nope",
        Some(StatementEnd::Failed(ExecuteError::Timeout)),
    );
    assert_eq!(displayed(&[ok, bad], None), Some(1));
    let (ok, bad) = (
        ended(0, "SELECT 1", None),
        ended(
            1,
            "SELECT nope",
            Some(StatementEnd::Failed(ExecuteError::Timeout)),
        ),
    );
    assert_eq!(displayed(&[ok, bad], Some(0)), Some(0), "a pick wins");
}

#[gpui_kit::test]
fn run_all_that_fails_shows_the_failed_statement(cx: &mut TestAppContext) {
    let o = opened(cx, T, "", 0);
    o.run(cx, "SELECT x FROM t; SELECT * FROM nope");
    o.idle(cx);
    assert!(present(cx, o.win, "result-error"));
}

#[gpui_kit::test]
fn an_explain_waiting_on_its_question_does_not_say_explaining(cx: &mut TestAppContext) {
    init(cx);
    let pending = PlanResult::new(ExplainMode::Analyze, false);
    let win = cx.add_window(|window, cx| PlanView::new(&pending, window, cx).waiting(true));
    assert_eq!(
        label_of(cx, win.into(), "plan-pending").as_deref(),
        Some("Waiting for confirmation")
    );
    let win = cx.add_window(|window, cx| PlanView::new(&pending, window, cx).waiting(false));
    assert_eq!(
        label_of(cx, win.into(), "plan-pending").as_deref(),
        Some("Explaining…")
    );
}
