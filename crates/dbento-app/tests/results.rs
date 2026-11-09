use crate::support;
use crate::support::{click_id, label_of, opened, present};
use dbento_app::results::ResultStore;
use dbento_app::results_pane::displayed;
use dbento_app::runner::StatementEnd;
use dbento_core::CellRef;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext, px};

fn store(sql: &str) -> (ResultStore, usize) {
    let (columns, batches) = support::batches(sql);
    let n = batches.len();
    let mut s = ResultStore::new(columns);
    for b in batches {
        s.push(b);
    }
    (s, n)
}
#[test]
fn cells_map_across_batches() {
    let (s, n) = store(support::MANY);
    assert!(n > 1, "MANY must span several 1,024-row batches");
    assert_eq!(s.rows(), 5000);
    assert_eq!(s.columns()[0].name(), "x");
    for row in [0u64, 1023, 1024, 2047, 4999] {
        assert_eq!(s.cell(row, 0), Some(CellRef::Text(&(row + 1).to_string())));
    }
    assert_eq!(s.cell(5000, 0), None);
    assert_eq!(s.cell(0, 1), None);
}
#[test]
fn is_number_follows_a_cell_into_a_later_batch_and_through_a_local_sort() {
    // Every value is an integer except 1500, stored as text, which lands in the second batch.
    let (mut s, n) = store(
        "WITH RECURSIVE r(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM r WHERE x < 2000) \
         SELECT CASE WHEN x = 1500 THEN CAST(x AS TEXT) ELSE x END AS v FROM r",
    );
    assert!(n > 1, "the result must span several batches");
    assert_eq!(s.cell(1499, 0), Some(CellRef::Text("1500")));
    assert!(!s.is_number(1499, 0));
    assert!(s.is_number(1498, 0) && s.is_number(1500, 0));
    // Descending, 1500 moves to shown row 500, inside the first batch's range.
    s.sort_local(0, true);
    assert_eq!(s.cell(500, 0), Some(CellRef::Text("1500")));
    assert!(!s.is_number(500, 0));
    assert!(s.is_number(499, 0) && s.is_number(501, 0) && s.is_number(1499, 0));
}
#[test]
fn null_bytes_and_empty_text_stay_typed() {
    let (s, _) = store("SELECT NULL AS n, x'0102' AS b, '' AS e");
    assert_eq!(s.cell(0, 0), Some(CellRef::Null));
    assert_eq!(s.cell(0, 1), Some(CellRef::Bytes(&[1, 2])));
    assert_eq!(s.cell(0, 2), Some(CellRef::Text("")));
}
#[test]
fn an_empty_result_keeps_its_columns_and_records_its_end() {
    let (mut s, _) = store("SELECT 1 AS a WHERE 0");
    assert_eq!((s.rows(), s.columns().len(), s.cell(0, 0)), (0, 1, None));
    assert!(s.end().is_none());
    s.finish(StatementEnd::Skipped);
    assert!(matches!(s.end(), Some(StatementEnd::Skipped)));
}

#[test]
fn heap_bytes_is_the_sum_of_its_batches() {
    let (columns, batches) = support::batches(support::MANY);
    let want: usize = batches.iter().map(|b| b.heap_bytes()).sum();
    let mut s = ResultStore::new(columns);
    assert_eq!(s.heap_bytes(), 0);
    for b in batches {
        s.push(b);
    }
    assert_eq!(s.heap_bytes(), want);
    assert!(want >= 5000 * 4, "every row keeps at least its end offset");
}

const AB: &str = "SELECT 1 AS a; SELECT 2 AS b";
#[gpui_kit::test]
fn clicking_a_result_tab_shows_that_statement(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    o.run(cx, AB);
    o.idle(cx);
    assert_eq!(
        label_of(cx, o.win, ("result-col", 0usize)).as_deref(),
        Some("b")
    );
    click_id(cx, o.win, ("statement-result", 0usize));
    assert_eq!(
        label_of(cx, o.win, ("result-col", 0usize)).as_deref(),
        Some("a")
    );
    assert_eq!(
        label_of(cx, o.win, ("statement-result", 0usize)).as_deref(),
        Some("1 row")
    );
}
#[gpui_kit::test]
fn a_new_run_resets_the_selection(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    o.run(cx, AB);
    o.idle(cx);
    click_id(cx, o.win, ("statement-result", 0usize));
    // A stale Some(0) would show c; the reset shows the last result, d.
    o.run(cx, "SELECT 3 AS c; SELECT 4 AS d");
    o.idle(cx);
    assert_eq!(
        label_of(cx, o.win, ("result-col", 0usize)).as_deref(),
        Some("d")
    );
}
#[gpui_kit::test]
fn the_strip_is_32_px_and_keeps_every_tab_when_it_overflows(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let sql = (1..=12)
        .map(|i| format!("SELECT {i} AS c{i}"))
        .collect::<Vec<_>>()
        .join("; ");
    o.run(cx, &sql);
    o.idle(cx);
    for i in 0..12usize {
        assert!(present(cx, o.win, ("statement-result", i)), "{i}");
    }
    cx.update_window(o.win, |_, w, cx| {
        w.render_frame(cx);
        assert_eq!(w.find("statement-list").bounds().size.height, px(32.));
    })
    .unwrap();
}
#[gpui_kit::test]
fn the_footer_names_rows_and_time(cx: &mut TestAppContext) {
    let o = opened(
        cx,
        "CREATE TABLE t(x); INSERT INTO t VALUES (1),(2),(3);",
        "",
        0,
    );
    o.run(cx, "SELECT x FROM t");
    o.idle(cx);
    assert_eq!(
        label_of(cx, o.win, "footer-rows").as_deref(),
        Some("3 rows")
    );
    let t = label_of(cx, o.win, "footer-elapsed").unwrap();
    assert!(t.ends_with(" ms") || t.ends_with(" s"), "{t}");
    cx.update_window(o.win, |_, w, cx| {
        w.render_frame(cx);
        assert_eq!(w.find("result-footer").bounds().size.height, px(28.));
    })
    .unwrap();
}
#[test]
fn displayed_ignores_a_selection_past_the_results() {
    assert_eq!(displayed(&[], Some(0)), None);
    assert_eq!(displayed(&[], None), None);
}
// With no statement selected and none with columns, the body and footer fall back to the last.
#[gpui_kit::test]
fn a_write_only_run_footer_describes_the_last_statement(cx: &mut TestAppContext) {
    let o = opened(cx, "CREATE TABLE t(x);", "", 0);
    o.run(
        cx,
        "INSERT INTO t VALUES (1); INSERT INTO t VALUES (2), (3)",
    );
    o.idle(cx);
    assert!(!present(cx, o.win, "footer-rows"));
    assert_eq!(
        label_of(cx, o.win, "footer-note").as_deref(),
        Some("2 rows affected")
    );
}
#[gpui_kit::test]
fn selecting_a_write_statement_shows_an_empty_grid_and_its_footer(cx: &mut TestAppContext) {
    let o = opened(cx, "CREATE TABLE t(x);", "", 0);
    o.run(cx, "INSERT INTO t VALUES (1); SELECT x FROM t");
    o.idle(cx);
    click_id(cx, o.win, ("statement-result", 0usize));
    assert!(!present(cx, o.win, ("result-col", 0usize)));
    // A write's footer names what it changed instead of counting no rows.
    assert!(!present(cx, o.win, "footer-rows"));
    assert_eq!(
        label_of(cx, o.win, "footer-note").as_deref(),
        Some("1 row affected")
    );
    click_id(cx, o.win, ("statement-result", 1usize));
    assert_eq!(
        label_of(cx, o.win, ("result-col", 0usize)).as_deref(),
        Some("x")
    );
}
