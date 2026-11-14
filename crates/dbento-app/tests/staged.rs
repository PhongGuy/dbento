//! The staged edit set and the words the save flow shows.
use crate::support::{edit_target, fixture};
use dbento_app::staged::{
    GridRow, PREVIEW_LINES, RowEdit, StagedEdits, commit_error_text, edit_value, outcome_text,
    pending_label, preview, prompt_detail, prompt_title, rejection_text, row_key,
};
use dbento_core::{
    ApplyError, CellRef, Change, ChangeOutcome, EditBatch, EditValue, ExecuteError, RelationRef,
    RowKey,
};

fn key(id: &str) -> RowKey {
    RowKey::new(vec![("id".into(), EditValue::Text(id.into()))])
}
fn text(s: &str) -> EditValue {
    EditValue::Text(s.into())
}

#[test]
fn staging_the_loaded_value_again_unstages_the_cell() {
    let mut s = StagedEdits::default();
    assert!(s.stage(0, key("1"), 1, "name", &text("b"), text("z")));
    assert_eq!(s.cell(0, 1), Some(&text("z")));
    assert_eq!(s.len(), 1);
    assert!(s.stage(0, key("1"), 1, "name", &text("b"), text("b")));
    assert_eq!(s.cell(0, 1), None);
    assert!(s.is_empty());
    assert_eq!(s.row(0), None);
}

#[test]
fn one_change_per_row_with_set_columns_in_grid_order() {
    let mut s = StagedEdits::default();
    s.stage(2, key("3"), 2, "note", &EditValue::Null, text("n"));
    s.stage(2, key("3"), 1, "name", &text("c"), text("x"));
    s.stage(0, key("1"), 1, "name", &text("b"), text("y"));
    assert_eq!(s.len(), 2);
    let (changes, rows) = s.changes();
    assert_eq!(rows, vec![GridRow::Loaded(0), GridRow::Loaded(2)]);
    assert_eq!(
        changes,
        vec![
            Change::Update {
                key: key("1"),
                set: vec![("name".into(), text("y"))],
            },
            Change::Update {
                key: key("3"),
                set: vec![("name".into(), text("x")), ("note".into(), text("n"))],
            },
        ]
    );
}

#[test]
fn a_delete_replaces_the_rows_cell_edits_and_refuses_later_ones() {
    let mut s = StagedEdits::default();
    s.stage(1, key("2"), 1, "name", &text("a"), text("q"));
    s.delete(1, key("2"));
    assert_eq!(s.row(1), Some(&RowEdit::Delete { key: key("2") }));
    assert!(s.is_deleted(1));
    assert_eq!(s.cell(1, 1), None);
    assert!(!s.stage(1, key("2"), 1, "name", &text("a"), text("r")));
    assert_eq!(s.changes().0, vec![Change::Delete { key: key("2") }]);
    assert_eq!(s.len(), 1);
}

#[test]
fn outcomes_mark_their_rows_until_cleared() {
    let mut s = StagedEdits::default();
    s.stage(0, key("1"), 1, "name", &text("b"), text("y"));
    s.delete(4, key("5"));
    s.delete(6, key("7"));
    let (_, rows) = s.changes();
    s.set_outcomes(
        &rows,
        &[
            ChangeOutcome::Applied(1),
            ChangeOutcome::Conflict,
            ChangeOutcome::Failed("boom".into()),
        ],
    );
    assert_eq!(s.mark(0), None);
    assert_eq!(s.mark(4), Some("modified or deleted by someone else"));
    assert_eq!(s.mark(6), Some("boom"));
    s.clear_marks();
    assert_eq!(s.mark(4), None);
    assert_eq!(s.len(), 3);
    s.set_outcomes(
        &rows,
        &[
            ChangeOutcome::Ambiguous(2),
            ChangeOutcome::NotAttempted,
            ChangeOutcome::NotAttempted,
        ],
    );
    assert_eq!(s.mark(0), Some("matched 2 rows"));
    assert_eq!(s.mark(4), None);
    s.clear();
    assert!(s.is_empty());
    assert_eq!(s.mark(0), None);
}

#[test]
fn a_row_key_reads_the_identity_cells_by_name_in_identity_order() {
    let row = [
        ("name", CellRef::Text("b")),
        ("b", CellRef::Null),
        ("a", CellRef::Bytes(&[1, 2])),
    ];
    let identity = vec!["a".to_owned(), "b".to_owned()];
    assert_eq!(
        row_key(&identity, &row),
        Some(RowKey::new(vec![
            ("a".into(), EditValue::Bytes(vec![1, 2])),
            ("b".into(), EditValue::Null),
        ]))
    );
    assert_eq!(row_key(&["id".to_owned()], &row), None);
    assert_eq!(edit_value(CellRef::Text("x")), Some(text("x")));
    assert_eq!(edit_value(CellRef::Null), Some(EditValue::Null));
    assert_eq!(
        edit_value(CellRef::Bytes(&[7])),
        Some(EditValue::Bytes(vec![7]))
    );
}

#[test]
fn staged_changes_make_a_valid_batch() {
    let fx = fixture(
        "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT); INSERT INTO t VALUES (1, 'b');",
        "",
    );
    let mut s = StagedEdits::default();
    s.stage(0, key("1"), 1, "name", &text("b"), text("z"));
    s.delete(3, key("4"));
    assert!(EditBatch::new(edit_target(&fx, "t"), s.changes().0).is_ok());
}

#[test]
fn the_save_flow_words() {
    assert_eq!(pending_label(1), "1 pending change");
    assert_eq!(pending_label(3), "3 pending changes");
    assert_eq!(
        prompt_title(&RelationRef::new("public", "film")),
        "Apply changes to public.film"
    );
    assert_eq!(
        prompt_detail(1),
        "Affects 1 row. Locks are held until you choose."
    );
    assert_eq!(
        prompt_detail(4),
        "Affects 4 rows. Locks are held until you choose."
    );
    assert_eq!(outcome_text(&ChangeOutcome::Applied(1)), None);
    assert_eq!(outcome_text(&ChangeOutcome::NotAttempted), None);
    assert_eq!(
        outcome_text(&ChangeOutcome::Conflict).as_deref(),
        Some("modified or deleted by someone else")
    );
    assert_eq!(
        outcome_text(&ChangeOutcome::Ambiguous(3)).as_deref(),
        Some("matched 3 rows")
    );
}

#[test]
fn rejection_names_the_first_failure_else_the_conflicts() {
    use ChangeOutcome::*;
    assert_eq!(
        rejection_text(&[Applied(1), Conflict, Conflict]),
        "Nothing was committed: 2 rows were modified by someone else"
    );
    assert_eq!(
        rejection_text(&[Conflict]),
        "Nothing was committed: 1 row was modified by someone else"
    );
    assert_eq!(
        rejection_text(&[Conflict, Failed("bad input".into()), NotAttempted]),
        "Nothing was committed: bad input"
    );
    assert_eq!(
        rejection_text(&[Applied(1), Ambiguous(3)]),
        "Nothing was committed: 1 change matched more than one row"
    );
    assert_eq!(
        rejection_text(&[Ambiguous(2), Ambiguous(2)]),
        "Nothing was committed: 2 changes matched more than one row"
    );
}

#[test]
fn a_refused_commit_and_a_lost_one_read_differently() {
    let refused = ApplyError::CommitFailed(ExecuteError::database("23503", "deferred fk"));
    assert_eq!(
        commit_error_text(&refused),
        "Commit failed: deferred fk. Nothing was committed."
    );
    let lost = ApplyError::CommitUnknown(ExecuteError::SessionClosed);
    assert_eq!(
        commit_error_text(&lost),
        "The connection closed during COMMIT, so whether the changes were saved is unknown. \
         Run the table query again to check."
    );
}

#[test]
fn a_commit_refused_for_a_lock_names_who_may_hold_it() {
    let locked = "Commit failed: another connection still holds the database file (a query or \
                  transaction open in another tab, or another program). Nothing was committed.";
    for code in ["5", "261", "517", "773"] {
        let busy = ApplyError::CommitFailed(ExecuteError::database(code, "database is locked"));
        assert_eq!(commit_error_text(&busy), locked, "SQLite code {code}");
    }
}

#[test]
fn the_preview_keeps_the_first_statements_and_counts_the_rest() {
    let sql = (0..PREVIEW_LINES + 5)
        .map(|i| format!("DELETE FROM t WHERE id = {i};"))
        .collect::<Vec<_>>()
        .join("\n");
    let shown = preview(&sql);
    assert_eq!(shown.lines().count(), PREVIEW_LINES + 1);
    assert_eq!(shown.lines().next(), Some("DELETE FROM t WHERE id = 0;"));
    assert_eq!(shown.lines().last(), Some("-- and 5 more statements"));
    let one_more = (0..=PREVIEW_LINES)
        .map(|i| format!("S{i};"))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        preview(&one_more).lines().last(),
        Some("-- and 1 more statement")
    );
    assert_eq!(preview("A;\nB;"), "A;\nB;");
}

#[test]
fn the_preview_shows_built_in_casts_unqualified_and_leaves_literals_alone() {
    let sql = "UPDATE ONLY public.t SET a = 'x::pg_catalog.text'::pg_catalog.text, \
               n = '1.5'::pg_catalog.\"numeric\", c = 'y'::pg_catalog.\"char\", \
               m = 'ok'::public.mood, \"pg_catalog.text\" = E'\\\\'''::pg_catalog.\"varchar\" \
               WHERE ctid = '(0,2)'::pg_catalog.tid;";
    assert_eq!(
        preview(sql),
        "UPDATE ONLY public.t SET a = 'x::pg_catalog.text'::text, \
         n = '1.5'::numeric, c = 'y'::\"char\", \
         m = 'ok'::public.mood, \"pg_catalog.text\" = E'\\\\'''::varchar \
         WHERE ctid = '(0,2)'::tid;"
    );
}
