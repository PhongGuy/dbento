//! A failed statement fills the result body; Jump puts the caret on the error.
use crate::support;
use std::sync::Arc;

use crate::support::{Opened, fixture, label_of, opened, present, spec, wait_real};
use dbento_app::result_body::{ErrorView, code_label, error_copy_text, error_view, jump_target};
use dbento_app::results::ResultStore;
use dbento_app::runner::StatementEnd;
use dbento_app::tab_model::{StatementResult, TabModel, statement_source};
use dbento_core::{Engine, ExecuteError};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, Entity, TestAppContext};

fn model(cx: &mut TestAppContext, fx: &support::Fx) -> Entity<TabModel> {
    support::init_tokio(cx);
    cx.new(|_| TabModel::new(spec(fx, "lite")))
}
fn ran(cx: &mut TestAppContext, m: &Entity<TabModel>) {
    wait_real(cx, "the run", |cx| m.read_with(cx, |m, _| !m.running()));
}
fn click(cx: &mut TestAppContext, o: &Opened, id: &'static str) {
    cx.update_window(o.win, |_, w, cx| w.click(id, cx)).unwrap();
    cx.run_until_parked();
}

#[test]
fn a_source_skips_leading_whitespace_and_counts_from_the_run_start() {
    let text = "SELECT 1;\n\n  -- note\nSELECT 2;";
    assert_eq!(statement_source(Engine::Sqlite, text, 0, 0), Some(0..9));
    let second = text.find("-- note").unwrap();
    assert_eq!(
        statement_source(Engine::Sqlite, text, 100, 1),
        Some(100 + second..100 + text.len())
    );
    assert_eq!(statement_source(Engine::Sqlite, text, 0, 2), None);
    let tail = "SELECT 1;\nSELECT 2\n";
    assert_eq!(
        statement_source(Engine::Sqlite, tail, 0, 1),
        Some(10..18),
        "trailing whitespace is not part of the statement"
    );
}

#[gpui_kit::test]
fn jump_lands_on_the_token_and_refuses_a_changed_editor(cx: &mut TestAppContext) {
    let fx = fixture("CREATE TABLE t(a);", "");
    let m = model(cx, &fx);
    let editor = "-- head\nSELECT 1;\n-- why\nSELECT nope_col FROM t";
    let start = "-- head\n".len();
    m.update(cx, |m, cx| m.run_at(editor[start..].to_owned(), start, cx));
    ran(cx, &m);
    m.read_with(cx, |m, _| {
        let r = &m.results()[1];
        assert_eq!(r.source, Some(editor.find("-- why").unwrap()..editor.len()));
        assert_eq!(
            jump_target(editor, r),
            Some(editor.find("nope_col").unwrap())
        );
        assert_eq!(
            jump_target(&editor.replace("nope_col", "nope_coI"), r),
            None
        );
        assert_eq!(
            jump_target(editor, &m.results()[0]),
            None,
            "no error, no jump"
        );
    });
}

#[gpui_kit::test]
fn the_error_view_holds_message_code_and_statement(cx: &mut TestAppContext) {
    let fx = fixture("CREATE TABLE t(a);", "");
    let m = model(cx, &fx);
    m.update(cx, |m, cx| m.run("SELECT nope_col FROM t".into(), cx));
    ran(cx, &m);
    let v = m.read_with(cx, |m, _| error_view(&m.results()[0])).unwrap();
    assert!(v.message.contains("nope_col"), "{}", v.message);
    assert_eq!(v.code.as_deref(), Some("1"));
    assert_eq!(
        (v.position, v.sql.as_str()),
        (Some(7), "SELECT nope_col FROM t")
    );
    assert_eq!(
        error_copy_text(&v),
        format!("{}\nCode: 1\n\nSELECT nope_col FROM t", v.message)
    );
    assert_eq!(v.sqlstate, None);
    assert_eq!(
        code_label(Some(Engine::Sqlite), &v).as_deref(),
        Some("code 1")
    );
    let pg = ErrorView {
        code: Some("23505".into()),
        ..v.clone()
    };
    assert_eq!(
        code_label(Some(Engine::Postgres), &pg).as_deref(),
        Some("SQLSTATE 23505")
    );
    let bare = ErrorView {
        code: Some("1317".into()),
        ..v.clone()
    };
    assert_eq!(
        code_label(Some(Engine::Mysql), &bare).as_deref(),
        Some("error 1317")
    );
}

#[gpui_kit::test]
fn a_mysql_error_shows_its_number_and_sqlstate(cx: &mut TestAppContext) {
    support::init(cx);
    support::init_tokio(cx);
    let rt = cx.update(|cx| dbento_app::tokio_bridge::handle(cx));
    let _enter = rt.enter();
    let db = rt.block_on(dbento_testkit::mysql_db());
    let (_dir, spec) = support::my_spec(&db, "");
    let m = cx.new(|_| TabModel::new(spec));
    m.update(cx, |m, cx| m.run("SELECT * FROM nope".into(), cx));
    ran(cx, &m);
    let v = m.read_with(cx, |m, _| error_view(&m.results()[0])).unwrap();
    assert_eq!(
        (v.code.as_deref(), v.sqlstate.as_deref()),
        (Some("1146"), Some("42S02"))
    );
    assert!(v.message.contains("nope"), "{}", v.message);
    assert_eq!(
        code_label(Some(Engine::Mysql), &v).as_deref(),
        Some("error 1146 \u{b7} SQLSTATE 42S02")
    );
    let copied = error_copy_text(&v);
    assert!(
        copied.contains("\nCode: 1146\nSQLSTATE: 42S02\n"),
        "{copied}"
    );
}

#[gpui_kit::test]
fn a_failed_run_fills_the_body_and_jump_moves_the_caret(cx: &mut TestAppContext) {
    let o = opened(cx, "CREATE TABLE t(a);", "", 0);
    let text = "\n-- why\nSELECT nope_col FROM t\n";
    o.run(cx, text);
    o.idle(cx);
    assert_eq!(
        label_of(cx, o.win, "result-error").as_deref(),
        Some("Query failed")
    );
    assert!(
        label_of(cx, o.win, "result-error-message")
            .unwrap()
            .contains("nope_col")
    );
    assert_eq!(
        label_of(cx, o.win, "result-error-code").as_deref(),
        Some("code 1")
    );
    assert!(
        present(cx, o.win, "result-grid"),
        "the body sits inside the grid area"
    );
    assert!(
        !present(cx, o.win, "footer-rows"),
        "no row count beside a failure"
    );
    click(cx, &o, "result-error-copy");
    let copied = cx.read_from_clipboard().and_then(|c| c.text()).unwrap();
    assert!(
        copied.ends_with("\n\n-- why\nSELECT nope_col FROM t"),
        "{copied}"
    );
    click(cx, &o, "result-error-jump");
    let editor = o
        .shell
        .read_with(cx, |s, cx| s.tab().unwrap().read(cx).editor().clone());
    let at = text.find("nope_col").unwrap();
    assert_eq!(editor.read_with(cx, |e, _| e.selected_range()), at..at);
    let tab = o.shell.read_with(cx, |s, _| s.tab().unwrap().clone());
    cx.update_window(o.win, |_, w, cx| {
        tab.update(cx, |t, cx| t.set_sql(&text.replace("nope", "nop"), w, cx))
    })
    .unwrap();
    cx.run_until_parked();
    // The kit's snapshot exposes no disabled flag, so the disabled Jump is seen by its click
    // leaving the caret where it was.
    cx.update(|cx| editor.update(cx, |e, cx| e.set_selected_range(0..0, cx)));
    click(cx, &o, "result-error-jump");
    assert_eq!(
        editor.read_with(cx, |e, _| e.selected_range()),
        0..0,
        "Jump stops once the editor changed"
    );
}

#[gpui_kit::test]
fn a_later_failure_keeps_the_earlier_grid_until_its_tab_is_picked(cx: &mut TestAppContext) {
    let o = opened(cx, "CREATE TABLE t(a);", "", 0);
    o.run(cx, "SELECT 5 AS a; SELECT nope_col FROM t");
    o.idle(cx);
    assert_eq!(label_of(cx, o.win, "cell-0-0").as_deref(), Some("5"));
    assert!(!present(cx, o.win, "result-error"));
    cx.update_window(o.win, |_, w, cx| w.click(("statement-result", 1usize), cx))
        .unwrap();
    cx.run_until_parked();
    assert_eq!(
        label_of(cx, o.win, "result-error").as_deref(),
        Some("Query failed")
    );
}

#[gpui_kit::test]
fn on_postgres_jump_counts_the_comment_before_the_failing_statement(cx: &mut TestAppContext) {
    support::init_tokio(cx);
    let rt = cx.update(|cx| dbento_app::tokio_bridge::handle(cx));
    let _enter = rt.enter();
    let (_db, _dir, spec) = rt.block_on(support::pg_spec(""));
    let m = cx.new(|_| TabModel::new(spec));
    let editor = "SELECT 1;\n-- the café check\nSELECT 'é', nope_col";
    m.update(cx, |m, cx| m.run_at(editor.to_owned(), 0, cx));
    ran(cx, &m);
    m.read_with(cx, |m, _| {
        assert_eq!(
            jump_target(editor, &m.results()[1]),
            Some(editor.find("nope_col").unwrap())
        );
    });
}

#[test]
fn an_empty_database_code_shows_no_code() {
    let mut store = ResultStore::new(Arc::from(Vec::new()));
    store.finish(StatementEnd::Failed(ExecuteError::database("", "x")));
    let r = StatementResult {
        stmt: 0,
        sql: "SELECT 1".into(),
        engine: Some(Engine::Duckdb),
        epoch: 0,
        store,
        elapsed: None,
        source: None,
        plan: None,
    };
    let v = error_view(&r).unwrap();
    assert_eq!((v.message.as_str(), v.code.as_deref()), ("x", None));
    assert_eq!(code_label(Some(Engine::Duckdb), &v), None);
    assert_eq!(error_copy_text(&v), "x\n\nSELECT 1");
}

#[test]
fn a_surrealdb_result_maps_to_the_unit_that_holds_it() {
    // Results 1 to 3 come from the one transaction unit; result 4 is the last SELECT.
    let text = "SELECT 1; BEGIN; CREATE a:1; COMMIT; SELECT 2";
    let e = Engine::Surrealdb;
    assert_eq!(statement_source(e, text, 0, 0), Some(0..9));
    for stmt in 1..=3 {
        assert_eq!(statement_source(e, text, 0, stmt), Some(10..36), "{stmt}");
    }
    assert_eq!(statement_source(e, text, 5, 4), Some(42..50));
    assert_eq!(statement_source(e, text, 0, 5), None);
    // SQL engines keep one result per statement.
    assert_eq!(
        statement_source(Engine::Postgres, "SELECT 1; SELECT 2", 0, 1),
        Some(10..18)
    );
}
