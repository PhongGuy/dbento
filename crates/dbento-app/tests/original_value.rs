//! The value a staged cell was loaded with: its text rules, where the model reads it, the
//! per-cell revert, and the status bar segment.
use crate::support::{Opened, fixture, label_of, launched, present, ready, wait_real};
use dbento_app::record::{ORIGINAL_CAP, original_text};
use dbento_app::results_pane::ResultsPane;
use dbento_app::staged::{GridRow, was_text};
use dbento_core::{CellRef, EditValue, RelationKind};
use gpui_kit::{AppContext as _, Entity, Focusable as _, TestAppContext};

const T: &str = "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT, note TEXT); \
                 INSERT INTO t VALUES (1, 'b', ' pad '), (2, '', NULL), (3, 'c', 'n');";
const T_SQL: &str = "SELECT *\nFROM main.t\nORDER BY id ASC";

#[test]
fn original_text_marks_null_empty_and_edge_whitespace() {
    assert_eq!(ORIGINAL_CAP, 200);
    assert_eq!(original_text(CellRef::Null, 200), "NULL");
    assert_eq!(original_text(CellRef::Text(""), 200), "\"\"");
    assert_eq!(original_text(CellRef::Text("shipped"), 200), "shipped");
    assert_eq!(original_text(CellRef::Text(" pad "), 200), "\" pad \"");
    assert_eq!(
        original_text(CellRef::Text("x\n\ty\t"), 200),
        "\"x\\n\\ty\\t\""
    );
    assert_eq!(original_text(CellRef::Text("a\nb"), 200), "\"a\\nb\"");
}

#[test]
fn original_text_escapes_quotes_and_keeps_to_one_line() {
    assert_eq!(original_text(CellRef::Text("a\r\nb"), 200), "\"a\\r\\nb\"");
    assert_eq!(original_text(CellRef::Text("a\tb"), 200), "\"a\\tb\"");
    assert_eq!(original_text(CellRef::Text(" a\"b "), 200), "\" a\\\"b \"");
    assert_eq!(original_text(CellRef::Text("\"q\""), 200), "\"\\\"q\\\"\"");
    assert_eq!(original_text(CellRef::Text("\"\""), 200), "\"\\\"\\\"\"");
    assert_eq!(original_text(CellRef::Text(" a\\b "), 200), "\" a\\\\b \"");
    assert_eq!(
        original_text(CellRef::Text("say \"hi\" now"), 200),
        "say \"hi\" now",
        "an inner quote is unambiguous"
    );
}

#[test]
fn a_size_that_rounds_to_1024_kb_reads_in_mb() {
    let size = |n: usize| original_text(CellRef::Bytes(&vec![0u8; n]), 200);
    assert!(size(1023).starts_with("BLOB \u{b7} 1023 bytes \u{b7}"));
    assert!(size(1024).starts_with("BLOB \u{b7} 1.0 KB \u{b7}"));
    assert!(size(1_048_524).starts_with("BLOB \u{b7} 1023.9 KB \u{b7}"));
    for n in [1_048_525, 1_048_575, 1_048_576] {
        assert!(
            size(n).starts_with("BLOB \u{b7} 1.0 MB \u{b7}"),
            "{n}: {}",
            size(n)
        );
    }
}

#[test]
fn original_text_caps_long_text_and_summarises_bytes() {
    let long = "x".repeat(250);
    assert_eq!(
        original_text(CellRef::Text(&long), 200),
        format!("{}\u{2026}", "x".repeat(200))
    );
    let wide = "\u{e9}".repeat(200);
    assert_eq!(
        original_text(CellRef::Text(&wide), 200),
        wide,
        "chars, not bytes"
    );
    let png = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0];
    assert_eq!(
        original_text(CellRef::Bytes(&png), 200),
        "BLOB \u{b7} 10 bytes \u{b7} \\x89504e470d0a1a0a\u{2026}"
    );
    assert_eq!(
        original_text(CellRef::Bytes(&[0xab]), 200),
        "BLOB \u{b7} 1 byte \u{b7} \\xab"
    );
    assert_eq!(
        original_text(CellRef::Bytes(&vec![0u8; 2355]), 200),
        "BLOB \u{b7} 2.3 KB \u{b7} \\x0000000000000000\u{2026}"
    );
    assert_eq!(
        original_text(CellRef::Bytes(&vec![0u8; 3 * 1024 * 1024]), 200),
        "BLOB \u{b7} 3.0 MB \u{b7} \\x0000000000000000\u{2026}"
    );
    assert_eq!(
        original_text(CellRef::Bytes(&[]), 200),
        "BLOB \u{b7} 0 bytes"
    );
}

#[test]
fn was_text_labels_the_loaded_value() {
    assert_eq!(was_text(CellRef::Text("pending")), "Was: pending");
    assert_eq!(was_text(CellRef::Null), "Was: NULL");
    assert_eq!(was_text(CellRef::Text("")), "Was: \"\"");
}

#[gpui_kit::test]
fn the_original_is_the_loaded_value_not_the_last_staged_step(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = ready(cx, &fx, "t");
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 1, "x", cx)));
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 1, "y", cx)));
    m.read_with(cx, |m, _| {
        assert_eq!(
            m.original_at(GridRow::Loaded(0), 1),
            Some(CellRef::Text("b"))
        );
        assert_eq!(
            m.original_at(GridRow::Loaded(1), 1),
            Some(CellRef::Text(""))
        );
        assert_eq!(m.original_at(GridRow::Loaded(1), 2), Some(CellRef::Null));
        assert_eq!(m.original_at(GridRow::Loaded(9), 1), None);
    });
    let ix = m.update(cx, |m, cx| m.add_row(cx)).unwrap();
    m.read_with(cx, |m, _| {
        assert_eq!(m.original_at(GridRow::Inserted(ix), 1), None)
    });
}

#[gpui_kit::test]
fn revert_cell_unstages_one_cell_in_one_undo_step(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = ready(cx, &fx, "t");
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 1, "x", cx)));
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 2, "y", cx)));
    assert!(m.update(cx, |m, cx| m.revert_cell(GridRow::Loaded(0), 1, cx)));
    m.read_with(cx, |m, _| {
        assert_eq!(m.edits().cell(0, 1), None);
        assert_eq!(m.edits().cell(0, 2), Some(&EditValue::Text("y".into())));
        assert_eq!(m.edits().len(), 1);
    });
    assert!(!m.update(cx, |m, cx| m.revert_cell(GridRow::Loaded(0), 1, cx)));
    assert!(m.update(cx, |m, cx| m.undo(cx)));
    m.read_with(cx, |m, _| {
        assert_eq!(m.edits().cell(0, 1), Some(&EditValue::Text("x".into())))
    });
    assert!(m.update(cx, |m, cx| m.revert_cell(GridRow::Loaded(0), 1, cx)));
    assert!(m.update(cx, |m, cx| m.revert_cell(GridRow::Loaded(0), 2, cx)));
    assert!(
        m.read_with(cx, |m, _| m.edits().is_empty()),
        "the last cell drops the row"
    );
    let ix = m.update(cx, |m, cx| m.add_row(cx)).unwrap();
    assert!(m.update(cx, |m, cx| m.stage_text(GridRow::Inserted(ix), 1, "n", cx)));
    assert!(!m.update(cx, |m, cx| m.revert_cell(GridRow::Inserted(ix), 1, cx)));
}

fn browsed(cx: &mut TestAppContext) -> Opened {
    let o = launched(cx, T, "");
    o.open_relation(cx, "t", RelationKind::Table);
    o.wait_browse(cx, T_SQL);
    o
}
fn pane(cx: &mut TestAppContext, o: &Opened) -> Entity<ResultsPane> {
    o.shell
        .read_with(cx, |s, cx| s.tab().unwrap().read(cx).results_pane().clone())
}
/// Selects grid cell (row, data column) and focuses the grid.
fn select(cx: &mut TestAppContext, o: &Opened, row: usize, data: usize) {
    let table = pane(cx, o).read_with(cx, |p, _| p.table().clone());
    cx.update_window(o.win, |_, w, cx| {
        table.update(cx, |t, cx| t.set_selected_cell(row, data + 1, cx));
        table.read(cx).focus_handle(cx).focus(w, cx);
    })
    .unwrap();
    cx.run_until_parked();
}

#[gpui_kit::test]
fn the_status_bar_repeats_the_original_of_the_selected_staged_cell(cx: &mut TestAppContext) {
    let o = browsed(cx);
    let m = o.model(cx);
    select(cx, &o, 0, 1);
    assert!(!present(cx, o.win, "status-was"), "nothing staged");
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 1, "z", cx)));
    assert_eq!(label_of(cx, o.win, "status-was").as_deref(), Some("Was: b"));
    assert!(m.update(cx, |m, cx| m.stage_cell(1, 1, "x", cx)));
    select(cx, &o, 1, 1);
    assert_eq!(
        label_of(cx, o.win, "status-was").as_deref(),
        Some("Was: \"\"")
    );
    select(cx, &o, 2, 1);
    assert!(!present(cx, o.win, "status-was"), "an unstaged cell");
}

#[gpui_kit::test]
fn the_status_bar_shows_a_multi_line_original_on_one_line(cx: &mut TestAppContext) {
    let o = launched(
        cx,
        "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT); INSERT INTO t VALUES (1, 'a\nb');",
        "",
    );
    o.open_relation(cx, "t", RelationKind::Table);
    o.wait_browse(cx, T_SQL);
    let m = o.model(cx);
    wait_real(cx, "an editable browse", |cx| {
        m.read_with(cx, |m, _| m.editable() && !m.running())
    });
    select(cx, &o, 0, 1);
    // The in-cell editor takes one line only, so the value is set whole.
    let z = EditValue::Text("z".into());
    assert!(m.update(cx, |m, cx| m.set_value(GridRow::Loaded(0), 1, z, cx)));
    assert_eq!(
        label_of(cx, o.win, "status-was").as_deref(),
        Some("Was: \"a\\nb\"")
    );
}
