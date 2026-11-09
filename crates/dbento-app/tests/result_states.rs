//! A write says what it changed; empty results say why they are empty.
use crate::support::{click_id, label_of, launched, opened, present, wait_real};
use dbento_app::keymap::{RUN_STATEMENT_KEYS, plus_keys};
use dbento_app::result_body::dml_text;
use dbento_core::RelationKind;
use gpui_kit::TestAppContext;

const T: &str = "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT); \
                 INSERT INTO t VALUES (1, 'b'), (2, 'a');";

#[test]
fn dml_text_names_the_verb() {
    assert_eq!(dml_text("UPDATE t SET a = 1", Some(1)), "1 row updated");
    assert_eq!(
        dml_text("  insert into t values (1), (2)", Some(2)),
        "2 rows inserted"
    );
    assert_eq!(dml_text("-- c\nDELETE FROM t", Some(0)), "0 rows deleted");
    assert_eq!(
        dml_text("WITH x AS (SELECT 1) UPDATE t SET a = 2", Some(3)),
        "3 rows affected"
    );
    assert_eq!(
        dml_text("CREATE TABLE u(a)", Some(0)),
        "Statement completed"
    );
    assert_eq!(dml_text("VACUUM", None), "Statement completed");
}

#[gpui_kit::test]
fn before_a_run_the_body_says_how_to_run(cx: &mut TestAppContext) {
    let o = opened(cx, T, "", 0);
    let want = format!("Run a statement with {}", plus_keys(RUN_STATEMENT_KEYS));
    assert_eq!(label_of(cx, o.win, "result-empty"), Some(want));
}

#[gpui_kit::test]
fn a_write_says_what_it_changed_and_a_select_says_no_rows(cx: &mut TestAppContext) {
    let o = opened(cx, T, "", 0);
    o.run(cx, "UPDATE t SET name = 'z' WHERE id = 1");
    o.idle(cx);
    assert_eq!(
        label_of(cx, o.win, "result-dml").as_deref(),
        Some("1 row updated")
    );
    assert!(!present(cx, o.win, "result-empty"));
    o.run(cx, "SELECT * FROM t WHERE id > 99");
    o.idle(cx);
    assert_eq!(
        label_of(cx, o.win, "result-empty").as_deref(),
        Some("No rows")
    );
    assert!(!present(cx, o.win, "result-dml"));
}

#[gpui_kit::test]
fn a_filter_without_matches_offers_clear_filter(cx: &mut TestAppContext) {
    let o = launched(cx, T, "");
    o.open_relation(cx, "t", RelationKind::Table);
    o.wait_browse(cx, "SELECT *\nFROM main.t\nORDER BY id ASC");
    o.model(cx)
        .update(cx, |m, cx| m.apply_filter("id > 99", cx));
    wait_real(cx, "the filtered run", |cx| {
        label_of(cx, o.win, "result-empty").as_deref() == Some("No rows match id > 99")
    });
    click_id(cx, o.win, "result-clear-filter");
    wait_real(cx, "the rows", |cx| {
        label_of(cx, o.win, "cell-0-0").as_deref() == Some("1")
    });
    assert!(!present(cx, o.win, "result-empty"));
}
