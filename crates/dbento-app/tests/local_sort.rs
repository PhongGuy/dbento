//! A query result sorts its loaded rows; a browse sets its server sort through the guard.
use crate::support;
use crate::support::{fixture, opened, ready, wait_real};
use dbento_app::results::ResultStore;
use dbento_app::tab_model::Discard;
use dbento_core::{CellRef, Sort};
use gpui_kit::{AppContext as _, TestAppContext};

const T: &str = "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT NOT NULL, note TEXT); \
                 INSERT INTO t VALUES (1, 'b', 'x'), (2, 'a', NULL);";
const V: &str = "SELECT * FROM (VALUES (2, 'b'), (10, 'a'), (NULL, 'c'), (1, 'd'), ('x', 'e'))";

fn store(sql: &str) -> ResultStore {
    let (columns, batches) = support::batches(sql);
    let mut s = ResultStore::new(columns);
    for b in batches {
        s.push(b);
    }
    s
}
fn col(s: &ResultStore, c: usize) -> Vec<String> {
    (0..s.rows())
        .map(|r| match s.cell(r, c) {
            Some(CellRef::Text(t)) => t.to_owned(),
            Some(CellRef::Null) => "NULL".to_owned(),
            _ => "?".to_owned(),
        })
        .collect()
}

#[test]
fn numbers_sort_as_numbers_then_text_then_null() {
    let mut s = store(V);
    s.sort_local(0, false);
    assert_eq!(col(&s, 0), ["1", "2", "10", "x", "NULL"]);
    assert_eq!(col(&s, 1), ["d", "b", "a", "e", "c"], "whole rows move");
    assert_eq!(s.local_sort(), Some((0, false)));
    s.sort_local(0, true);
    assert_eq!(col(&s, 0), ["NULL", "x", "10", "2", "1"]);
    s.clear_local_sort();
    assert_eq!(col(&s, 0), ["2", "10", "NULL", "1", "x"]);
    assert_eq!(s.local_sort(), None);
}

#[test]
fn rows_arriving_after_a_sort_land_in_order() {
    // SQLite closes batches at 1,024 rows, so the second batch arrives after the sort.
    let (columns, batches) = support::batches(
        "WITH RECURSIVE r(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM r WHERE x < 2000) SELECT x FROM r",
    );
    assert!(batches.len() >= 2);
    let mut batches = batches.into_iter();
    let mut s = ResultStore::new(columns);
    s.push(batches.next().unwrap());
    s.sort_local(0, true);
    assert_eq!(s.cell(0, 0), Some(CellRef::Text("1024")));
    for b in batches {
        s.push(b);
    }
    assert_eq!(s.rows(), 2000);
    assert_eq!(s.cell(0, 0), Some(CellRef::Text("2000")));
    assert_eq!(s.cell(1999, 0), Some(CellRef::Text("1")));
}

#[gpui_kit::test]
fn a_query_sorts_its_loaded_rows_until_the_next_run(cx: &mut TestAppContext) {
    let o = opened(cx, T, "", 0);
    o.run(cx, "SELECT id, name FROM t ORDER BY id");
    o.idle(cx);
    let m = o.model(cx);
    assert!(m.update(cx, |m, cx| m.sort_loaded(0, 1, Some(false), cx)));
    m.read_with(cx, |m, _| {
        let s = &m.results()[0].store;
        assert_eq!(s.cell(0, 1), Some(CellRef::Text("a")));
        assert_eq!(s.local_sort(), Some((1, false)));
    });
    assert!(
        !m.update(cx, |m, cx| m.sort_loaded(0, 1, Some(false), cx)),
        "unchanged"
    );
    assert!(
        !m.update(cx, |m, cx| m.sort_loaded(0, 9, Some(false), cx)),
        "no such column"
    );
    o.run(cx, "SELECT id, name FROM t ORDER BY id");
    o.idle(cx);
    m.read_with(cx, |m, _| {
        assert_eq!(m.results()[0].store.local_sort(), None)
    });
}

#[gpui_kit::test]
fn a_browse_sets_its_sort_through_the_guard(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = ready(cx, &fx, "t");
    assert!(
        !m.update(cx, |m, cx| m.sort_loaded(0, 1, Some(false), cx)),
        "a browse sorts on the server"
    );
    assert!(m.update(cx, |m, cx| m.set_sort(Some(Sort::new("name", true)), cx)));
    m.read_with(cx, |m, _| {
        let b = m.browse().unwrap();
        assert_eq!(b.sort_of("name"), Some(true));
        assert!(b.sql().contains("ORDER BY name DESC"), "{}", b.sql());
    });
    wait_real(cx, "the sorted browse", |cx| {
        m.read_with(cx, |m, _| m.editable() && !m.running())
    });
    assert!(
        !m.update(cx, |m, cx| m.set_sort(Some(Sort::new("name", true)), cx)),
        "unchanged"
    );
    assert!(!m.update(cx, |m, cx| m.set_sort(Some(Sort::new("nope", false)), cx)));
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 2, "z", cx)));
    assert!(!m.update(cx, |m, cx| m.set_sort(None, cx)));
    let q = m
        .read_with(cx, |m, _| m.discard_question().cloned())
        .unwrap();
    assert_eq!(q.action, Discard::SetSort(None));
    m.update(cx, |m, cx| m.answer_discard(q.id, true, cx));
    m.read_with(cx, |m, _| {
        assert!(m.browse().unwrap().browse().sort.is_none());
        assert!(m.edits().is_empty());
    });
}

#[gpui_kit::test]
fn a_local_sort_drops_the_selection_it_would_move(cx: &mut TestAppContext) {
    let o = opened(cx, T, "", 0);
    o.run(cx, "SELECT id, name FROM t ORDER BY id");
    o.idle(cx);
    let (m, pane) = (
        o.model(cx),
        o.shell
            .read_with(cx, |s, cx| s.tab().unwrap().read(cx).results_pane().clone()),
    );
    let select = |cx: &mut TestAppContext| {
        cx.update_window(o.win, |_, w, cx| {
            pane.update(cx, |p, cx| p.select_row(0, w, cx))
        })
        .unwrap();
        cx.run_until_parked();
        assert_eq!(pane.read_with(cx, |p, cx| p.current_row(cx)), Some(0));
    };
    select(cx);
    assert!(m.update(cx, |m, cx| m.sort_loaded(0, 1, Some(false), cx)));
    cx.run_until_parked();
    assert_eq!(
        pane.read_with(cx, |p, cx| p.current_row(cx)),
        None,
        "sorted"
    );
    select(cx);
    assert!(m.update(cx, |m, cx| m.sort_loaded(0, 1, None, cx)));
    cx.run_until_parked();
    assert_eq!(
        pane.read_with(cx, |p, cx| p.current_row(cx)),
        None,
        "restored"
    );
}

#[test]
fn nan_and_inf_text_sort_as_text() {
    let mut s = store(
        "SELECT * FROM (VALUES ('10'), ('NaN'), ('2'), ('inf'), ('-Infinity'), ('abc'), ('1e1'))",
    );
    s.sort_local(0, false);
    assert_eq!(
        col(&s, 0),
        ["2", "10", "1e1", "-Infinity", "NaN", "abc", "inf"]
    );
}

#[test]
fn infinite_reals_stored_as_numbers_still_sort_numerically() {
    // SQLite overflows 9e999 to an infinite REAL, which the driver renders as "inf".
    let mut s = store("SELECT * FROM (VALUES (9e999), (1.5), (-9e999), ('text'))");
    s.sort_local(0, false);
    assert_eq!(col(&s, 0), ["-inf", "1.5", "inf", "text"]);
}
