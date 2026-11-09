use crate::support;
use crate::support::opened;
use dbento_app::grid::{cell_display, is_numeric};
use dbento_app::results_pane::{ResultsPane, format_rows};
use dbento_app::tab_model::TabModel;
use dbento_core::CellRef;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext};

#[test]
fn row_counts_read_naturally() {
    assert_eq!(format_rows(0), "0 rows");
    assert_eq!(format_rows(1), "1 row");
    assert_eq!(format_rows(1000), "1,000 rows");
    assert_eq!(format_rows(1_234_567), "1,234,567 rows");
}

#[test]
fn cells_display_null_text_and_bytes() {
    assert_eq!(cell_display(CellRef::Null).0.as_ref(), "NULL");
    assert!(cell_display(CellRef::Null).1);
    assert_eq!(cell_display(CellRef::Text("a\nb")).0.as_ref(), "a");
    assert_eq!(
        cell_display(CellRef::Text(&"ü".repeat(300)))
            .0
            .chars()
            .count(),
        256
    );
    assert_eq!(
        cell_display(CellRef::Bytes(&[0xde, 0xad])).0.as_ref(),
        "\\xdead (2 bytes)"
    );
    assert_eq!(
        cell_display(CellRef::Bytes(&[0xff])).0.as_ref(),
        "\\xff (1 byte)"
    );
    let long = cell_display(CellRef::Bytes(&[0xab; 40])).0;
    assert_eq!(long.as_ref(), format!("\\x{}… (40 bytes)", "ab".repeat(32)));
}

fn pane(cx: &mut TestAppContext, setup: &str, sql: &str) -> gpui_kit::AnyWindowHandle {
    support::init(cx);
    support::init_tokio(cx);
    let fx = support::fixture(setup, "");
    let model = cx.new(|_| TabModel::new(support::spec(&fx, "lite")));
    let m = model.clone();
    let (win, _) = cx
        .update(|cx| {
            gpui_kit::open_window(Default::default(), cx, |window, cx| {
                cx.new(|cx| ResultsPane::new(m, window, cx))
            })
        })
        .unwrap();
    model.update(cx, |m, cx| m.run(sql.into(), cx));
    support::wait_real(cx, "the run", |cx| model.read_with(cx, |m, _| !m.running()));
    win
}
#[gpui_kit::test]
fn a_select_shows_columns_cells_and_a_summary(cx: &mut TestAppContext) {
    let win = pane(
        cx,
        "CREATE TABLE t(a, b); INSERT INTO t VALUES (1, NULL), (2, x'ff');",
        "SELECT a, b FROM t ORDER BY a",
    );
    assert_eq!(
        support::label_of(cx, win, ("result-col", 1usize)).as_deref(),
        Some("b")
    );
    assert_eq!(support::label_of(cx, win, "cell-1-0").as_deref(), Some("2"));
    assert_eq!(
        support::label_of(cx, win, "cell-0-1").as_deref(),
        Some("NULL")
    );
    assert_eq!(
        support::label_of(cx, win, "cell-1-1").as_deref(),
        Some("\\xff (1 byte)")
    );
    assert_eq!(
        support::label_of(cx, win, ("statement-result", 0usize)).as_deref(),
        Some("2 rows")
    );
    assert!(!support::present(cx, win, "result-message"));
}
#[gpui_kit::test]
fn a_script_lists_each_statement_and_shows_the_last_result(cx: &mut TestAppContext) {
    let sql =
        "INSERT INTO u VALUES (1); INSERT INTO u VALUES (2),(3); SELECT x FROM u ORDER BY x; SELEC";
    let win = pane(cx, "CREATE TABLE u(x);", sql);
    assert_eq!(
        support::label_of(cx, win, ("statement-result", 0usize)).as_deref(),
        Some("1 row affected")
    );
    assert_eq!(
        support::label_of(cx, win, ("statement-result", 1usize)).as_deref(),
        Some("2 rows affected")
    );
    assert_eq!(
        support::label_of(cx, win, ("statement-result", 2usize)).as_deref(),
        Some("3 rows")
    );
    let failed = support::label_of(cx, win, ("statement-result", 3usize)).unwrap();
    assert!(failed.contains("syntax error"), "{failed}");
    assert_eq!(support::label_of(cx, win, "cell-1-0").as_deref(), Some("2"));
}
#[test]
fn numeric_columns_follow_the_pg_oid_else_the_declared_type() {
    for oid in [20, 21, 23, 700, 701, 1700] {
        assert!(is_numeric("", Some(oid)), "{oid}");
    }
    // text: a numeric-looking declared type never overrides the oid.
    assert!(!is_numeric("INTEGER", Some(25)));
    for t in [
        "integer",
        "BIGINT",
        "real",
        "FLOAT",
        "double precision",
        "NUMERIC(10,2)",
        "decimal",
    ] {
        assert!(is_numeric(t, None), "{t}");
    }
    for t in ["", "TEXT", "BLOB", "DATE", "VARCHAR(5)"] {
        assert!(!is_numeric(t, None), "{t}");
    }
}

#[test]
fn rows_and_header_share_a_28_px_pitch() {
    let m = dbento_app::settings::Metrics::default();
    assert_eq!(
        (m.grid_row, m.tree_row),
        (gpui_kit::px(28.), gpui_kit::px(26.))
    );
}

#[gpui_kit::test]
fn rendered_rows_step_28_px_and_the_header_matches(cx: &mut TestAppContext) {
    let o = opened(
        cx,
        "CREATE TABLE t(x); INSERT INTO t VALUES (1),(2),(3);",
        "",
        0,
    );
    o.run(cx, "SELECT x FROM t");
    o.idle(cx);
    cx.update_window(o.win, |_, w, cx| {
        w.render_frame(cx);
        let (a, b) = (w.find("cell-0-0").bounds(), w.find("cell-1-0").bounds());
        assert_eq!(b.top() - a.top(), gpui_kit::px(28.));
        // The kit draws the header's hairline inside its 28 px row, so its pitch is measured
        // from the header's top to the first row rather than from the label's box.
        let head = w.find(("result-col", 0usize)).bounds();
        assert_eq!(a.top() - head.top(), gpui_kit::px(28.));
    })
    .unwrap();
}
