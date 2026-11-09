//! Inserted rows, Revert row and the undo history of the staged set.
use crate::support::{edit_target, fixture};
use dbento_app::staged::{EditHistory, GridRow, StagedEdits, UNDO_DEPTH, discard_title, grid_row};
use dbento_core::{Change, ChangeOutcome, EditBatch, EditValue, RowKey};

fn key(id: &str) -> RowKey {
    RowKey::new(vec![("id".into(), EditValue::Text(id.into()))])
}
fn text(s: &str) -> EditValue {
    EditValue::Text(s.into())
}

#[test]
fn table_rows_put_inserted_rows_above_the_loaded_ones() {
    assert_eq!(grid_row(0, 2), GridRow::Inserted(0));
    assert_eq!(grid_row(1, 2), GridRow::Inserted(1));
    assert_eq!(grid_row(2, 2), GridRow::Loaded(0));
    assert_eq!(grid_row(5, 0), GridRow::Loaded(5));
    assert_eq!(GridRow::Inserted(1).table_row(2), 1);
    assert_eq!(GridRow::Loaded(0).table_row(2), 2);
    assert_eq!(GridRow::Loaded(3).table_row(0), 3);
}

#[test]
fn an_added_row_holds_only_the_cells_given_and_counts_as_one_change() {
    let mut s = StagedEdits::default();
    assert_eq!(s.add_row(vec![]), 0);
    assert_eq!(s.add_row(vec![(1, "name".into(), text("n"))]), 1);
    assert_eq!(s.inserted(), 2);
    assert_eq!(s.len(), 2);
    assert!(!s.is_empty());
    assert_eq!(s.new_cell(0, 1), None, "an unset cell is DEFAULT");
    assert_eq!(s.new_cell(1, 1), Some(&text("n")));
    assert!(s.stage_new(0, 2, "note", EditValue::Null));
    assert_eq!(s.new_cell(0, 2), Some(&EditValue::Null));
    assert!(s.stage_new(0, 2, "note", EditValue::Default));
    assert_eq!(s.new_cell(0, 2), None, "DEFAULT drops the cell");
    assert!(
        !s.stage_new(7, 1, "name", text("x")),
        "no such inserted row"
    );
    s.clear();
    assert_eq!(s.inserted(), 0);
}

#[test]
fn inserts_follow_the_loaded_changes_in_the_order_added() {
    let mut s = StagedEdits::default();
    s.add_row(vec![(1, "name".into(), text("first"))]);
    s.stage(2, key("3"), 1, "name", &text("c"), text("x"));
    s.add_row(vec![]);
    s.delete(0, key("1"));
    let (changes, rows) = s.changes();
    assert_eq!(
        rows,
        vec![
            GridRow::Loaded(0),
            GridRow::Loaded(2),
            GridRow::Inserted(0),
            GridRow::Inserted(1),
        ]
    );
    assert_eq!(
        changes,
        vec![
            Change::Delete { key: key("1") },
            Change::Update {
                key: key("3"),
                set: vec![("name".into(), text("x"))],
            },
            Change::Insert {
                values: vec![("name".into(), text("first"))],
            },
            Change::Insert { values: vec![] },
        ]
    );
}

#[test]
fn removing_an_inserted_row_moves_the_later_ones_and_their_marks_up() {
    let mut s = StagedEdits::default();
    s.add_row(vec![(1, "name".into(), text("a"))]);
    s.add_row(vec![(1, "name".into(), text("b"))]);
    let (_, rows) = s.changes();
    s.set_outcomes(
        &rows,
        &[
            ChangeOutcome::NotAttempted,
            ChangeOutcome::Failed("boom".into()),
        ],
    );
    assert_eq!(s.new_mark(0), None);
    assert_eq!(s.new_mark(1), Some("boom"));
    assert!(s.remove_new(0));
    assert_eq!(s.inserted(), 1);
    assert_eq!(s.new_cell(0, 1), Some(&text("b")));
    assert_eq!(s.new_mark(0), Some("boom"));
    assert!(!s.remove_new(1));
}

#[test]
fn staging_a_marked_row_back_to_its_value_drops_the_mark_with_the_row() {
    let mut s = StagedEdits::default();
    s.stage(0, key("1"), 1, "name", &text("b"), text("y"));
    let (_, rows) = s.changes();
    s.set_outcomes(&rows, &[ChangeOutcome::Conflict]);
    assert!(s.stage(0, key("1"), 1, "name", &text("b"), text("b")));
    assert_eq!(s.row(0), None);
    assert_eq!(s.mark(0), None, "no mark outlives its row's edit");
    // So Revert row finds nothing to change, and the model records no undo step for it.
    let before = s.clone();
    assert!(!s.revert_row(0));
    assert_eq!(s, before);
}

#[test]
fn revert_row_drops_one_rows_edit_and_its_mark() {
    let mut s = StagedEdits::default();
    s.stage(0, key("1"), 1, "name", &text("b"), text("y"));
    s.delete(4, key("5"));
    let (_, rows) = s.changes();
    s.set_outcomes(
        &rows,
        &[ChangeOutcome::Conflict, ChangeOutcome::NotAttempted],
    );
    assert_eq!(s.mark(0), Some("modified or deleted by someone else"));
    assert!(s.revert_row(0));
    assert_eq!(s.row(0), None);
    assert_eq!(s.mark(0), None);
    assert!(s.is_deleted(4));
    assert!(!s.revert_row(0), "nothing left to revert");
    assert_eq!(s.len(), 1);
}

#[test]
fn undo_and_redo_walk_the_snapshots() {
    let mut h = EditHistory::default();
    let mut s = StagedEdits::default();
    assert!(!h.can_undo() && !h.can_redo());
    h.record(s.clone());
    s.stage(0, key("1"), 1, "name", &text("b"), text("y"));
    h.record(s.clone());
    s.add_row(vec![]);
    assert!(h.undo(&mut s));
    assert_eq!((s.len(), s.inserted()), (1, 0));
    assert!(h.undo(&mut s));
    assert_eq!(s, StagedEdits::default());
    assert!(!h.undo(&mut s), "nothing older");
    assert!(h.can_redo());
    assert!(h.redo(&mut s));
    assert_eq!(s.cell(0, 1), Some(&text("y")));
    assert!(h.redo(&mut s));
    assert_eq!(s.inserted(), 1);
    assert!(!h.redo(&mut s));
}

#[test]
fn a_new_step_drops_the_redo_and_the_depth_is_capped() {
    let mut h = EditHistory::default();
    let mut s = StagedEdits::default();
    h.record(s.clone());
    s.add_row(vec![]);
    assert!(h.undo(&mut s));
    h.record(s.clone());
    s.add_row(vec![(1, "name".into(), text("n"))]);
    assert!(!h.can_redo(), "a new step forgets the undone one");
    for _ in 0..UNDO_DEPTH + 5 {
        h.record(s.clone());
        s.add_row(vec![]);
    }
    let mut undone = 0;
    while h.undo(&mut s) {
        undone += 1;
    }
    assert_eq!(undone, UNDO_DEPTH);
    h.clear();
    assert!(!h.can_undo() && !h.can_redo());
}

#[test]
fn a_staged_insert_makes_a_valid_batch() {
    let fx = fixture("CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT);", "");
    let mut s = StagedEdits::default();
    s.add_row(vec![]);
    s.add_row(vec![(1, "name".into(), text("n"))]);
    assert!(EditBatch::new(edit_target(&fx, "t"), s.changes().0).is_ok());
}

#[test]
fn the_discard_question_counts_the_changes() {
    assert_eq!(discard_title(1), "Discard 1 pending change?");
    assert_eq!(discard_title(3), "Discard 3 pending changes?");
}
