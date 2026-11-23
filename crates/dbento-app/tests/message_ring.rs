//! A result store that grows at its top and trims its bottom, as a followed stream needs.
use crate::support;
use dbento_app::results::ResultStore;
use dbento_core::{CellRef, ColumnInfo, ResultBatch};
use std::sync::Arc;

/// One batch of text ids `n{from}`..=`n{to}`, ascending, or descending when `desc`.
fn ids(from: u32, to: u32, desc: bool) -> (Arc<[ColumnInfo]>, ResultBatch) {
    let order = if desc { "DESC" } else { "ASC" };
    let sql = format!(
        "WITH RECURSIVE r(x) AS (SELECT {from} UNION ALL SELECT x + 1 FROM r WHERE x < {to}) \
         SELECT 'n' || x AS id FROM r ORDER BY x {order}"
    );
    let (columns, mut batches) = support::batches(&sql);
    assert_eq!(batches.len(), 1, "one batch per call");
    (columns, batches.remove(0))
}
fn shown(s: &ResultStore) -> Vec<String> {
    (0..s.rows())
        .map(|r| match s.cell(r, 0) {
            Some(CellRef::Text(t)) => t.to_owned(),
            other => panic!("row {r}: {other:?}"),
        })
        .collect()
}
/// A Backward page n3..n1 (newest first), then a follow page above it.
fn followed() -> ResultStore {
    let (columns, page) = ids(1, 3, true);
    let mut s = ResultStore::new(columns);
    s.push(page);
    s.push_newest(ids(4, 6, false).1);
    s
}

#[test]
fn newest_rows_go_on_top_newest_first() {
    let s = followed();
    assert_eq!(shown(&s), ["n6", "n5", "n4", "n3", "n2", "n1"]);
}

#[test]
fn trimming_by_rows_drops_the_oldest_from_the_bottom() {
    let mut s = followed();
    assert_eq!(s.trim_oldest(4, usize::MAX), 2);
    assert_eq!(shown(&s), ["n6", "n5", "n4", "n3"]);
    // The bottom batch's last row goes whole, then the top batch loses its oldest row.
    assert_eq!(s.trim_oldest(2, usize::MAX), 2);
    assert_eq!(shown(&s), ["n6", "n5"]);
    assert_eq!(s.trim_oldest(2, usize::MAX), 0);
}

#[test]
fn trimming_by_bytes_drops_whole_batches_but_keeps_the_top() {
    let mut s = followed();
    let before = s.heap_bytes();
    assert_eq!(s.trim_oldest(u64::MAX, 1), 3);
    assert_eq!(shown(&s), ["n6", "n5", "n4"]);
    assert!(s.heap_bytes() < before);
    assert_eq!(s.trim_oldest(u64::MAX, 1), 0, "the top batch stays");
}

#[test]
fn growth_at_the_top_clears_a_local_sort_and_snapshots_keep_the_order() {
    let mut s = followed();
    s.sort_local(0, false);
    s.push_newest(ids(7, 7, false).1);
    assert_eq!(s.local_sort(), None);
    assert_eq!(
        shown(&s.snapshot()),
        ["n7", "n6", "n5", "n4", "n3", "n2", "n1"]
    );
    assert_eq!(s.max_chars(0), 2);
}
