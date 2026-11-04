//! Row operations on a browse tab's model: Set NULL and DEFAULT, add, duplicate, delete and
//! revert rows.
mod support;
use dbento_app::staged::GridRow::{Inserted, Loaded};
use dbento_core::EditValue;
use gpui_kit::TestAppContext;
use support::{browsing, fixture, ready, wait_real};

const T: &str = "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT NOT NULL DEFAULT 'anon', \
                 note TEXT, pic BLOB); \
                 INSERT INTO t VALUES (1, 'b', 'x', x'01'), (2, 'a', NULL, NULL);";

fn text(s: &str) -> EditValue {
    EditValue::Text(s.into())
}

#[gpui_kit::test]
fn set_null_and_default_stage_on_any_editable_cell(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = ready(cx, &fx, "t");
    assert_eq!(
        m.read_with(cx, |m, _| m.cell_text_at(Loaded(0), 3)),
        None,
        "bytes stay out of the editor"
    );
    assert!(
        m.update(cx, |m, cx| m.set_value(Loaded(0), 3, EditValue::Null, cx)),
        "a bytes cell can be set NULL"
    );
    assert!(m.update(cx, |m, cx| m.set_value(
        Loaded(0),
        1,
        EditValue::Default,
        cx
    )));
    m.read_with(cx, |m, _| {
        assert_eq!(m.edits().cell(0, 3), Some(&EditValue::Null));
        assert_eq!(m.edits().cell(0, 1), Some(&EditValue::Default));
        assert_eq!(
            m.cell_edit_text(0, 1).as_deref(),
            Some(""),
            "DEFAULT edits as empty text"
        );
        assert_eq!(m.settable(Loaded(0), 1), Some(EditValue::Default));
        assert_eq!(
            m.cell_text_at(Loaded(0), 3).as_deref(),
            Some(""),
            "a bytes cell set NULL edits as empty text"
        );
    });
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 3, "", cx)));
    assert_eq!(
        m.read_with(cx, |m, _| m.edits().cell(0, 3).cloned()),
        Some(EditValue::Null),
        "empty text over a shown NULL keeps NULL, never text in a BLOB"
    );
    assert!(
        m.update(cx, |m, cx| m.stage_cell(0, 1, "", cx)),
        "empty text over DEFAULT keeps it"
    );
    assert_eq!(
        m.read_with(cx, |m, _| m.edits().cell(0, 1).cloned()),
        Some(EditValue::Default)
    );
    assert!(m.update(cx, |m, cx| m.set_value(Loaded(1), 2, EditValue::Null, cx)));
    assert_eq!(
        m.read_with(cx, |m, _| m.edits().row(1).cloned()),
        None,
        "NULL over a loaded NULL is no change"
    );
    assert!(m.update(cx, |m, cx| m.delete_row(1, cx)));
    assert!(!m.update(cx, |m, cx| m.set_value(Loaded(1), 2, text("q"), cx)));
    assert_eq!(m.read_with(cx, |m, _| m.settable(Loaded(1), 2)), None);
}

#[gpui_kit::test]
fn an_added_row_starts_default_and_takes_typed_values(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = ready(cx, &fx, "t");
    assert_eq!(m.update(cx, |m, cx| m.add_row(cx)), Some(0));
    m.read_with(cx, |m, _| {
        assert_eq!(m.edits().inserted(), 1);
        assert_eq!(m.grid_row(0), Inserted(0));
        assert_eq!(m.grid_row(1), Loaded(0));
        assert_eq!(m.cell_text_at(Inserted(0), 1).as_deref(), Some(""));
        assert_eq!(m.settable(Inserted(0), 1), Some(EditValue::Default));
        assert!(m.has_change(Inserted(0)));
    });
    assert!(m.update(cx, |m, cx| m.stage_text(Inserted(0), 1, "", cx)));
    assert_eq!(
        m.read_with(cx, |m, _| m.edits().new_cell(0, 1).cloned()),
        None,
        "empty text keeps DEFAULT"
    );
    assert!(m.update(cx, |m, cx| m.stage_text(Inserted(0), 1, "new", cx)));
    assert_eq!(
        m.read_with(cx, |m, _| m.edits().new_cell(0, 1).cloned()),
        Some(text("new"))
    );
    assert!(m.update(cx, |m, cx| m.set_value(Inserted(0), 2, EditValue::Null, cx)));
    assert!(
        m.update(cx, |m, cx| m.stage_text(Inserted(0), 2, "", cx)),
        "empty text over an inserted NULL keeps NULL"
    );
    assert_eq!(
        m.read_with(cx, |m, _| m.edits().new_cell(0, 2).cloned()),
        Some(EditValue::Null)
    );
    assert!(!m.update(cx, |m, cx| m.stage_text(Inserted(3), 1, "x", cx)));
}

#[gpui_kit::test]
fn duplicate_copies_the_shown_values_but_not_the_key(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = ready(cx, &fx, "t");
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 2, "edited", cx)));
    assert_eq!(
        m.update(cx, |m, cx| m.duplicate_row(Loaded(0), cx)),
        Some(0)
    );
    m.read_with(cx, |m, _| {
        let e = m.edits();
        assert_eq!(e.new_cell(0, 0), None, "the primary key takes its default");
        assert_eq!(e.new_cell(0, 1), Some(&text("b")));
        assert_eq!(
            e.new_cell(0, 2),
            Some(&text("edited")),
            "the staged value wins"
        );
        assert_eq!(e.new_cell(0, 3), Some(&EditValue::Bytes(vec![1])));
        assert_eq!(
            m.cell_text_at(Inserted(0), 3),
            None,
            "bytes are not typed in the grid"
        );
    });
    assert_eq!(
        m.update(cx, |m, cx| m.duplicate_row(Inserted(0), cx)),
        Some(1)
    );
    assert_eq!(
        m.read_with(cx, |m, _| m.edits().new_cell(1, 2).cloned()),
        Some(text("edited"))
    );
}

#[gpui_kit::test]
fn delete_removes_an_inserted_row_and_revert_row_undoes_one_row(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = ready(cx, &fx, "t");
    m.update(cx, |m, cx| {
        assert_eq!(m.add_row(cx), Some(0));
        assert!(m.stage_cell(0, 1, "z", cx));
        assert!(m.delete_at(Loaded(1), cx));
        assert!(m.delete_at(Inserted(0), cx));
    });
    m.read_with(cx, |m, _| {
        assert_eq!(m.edits().inserted(), 0);
        assert!(m.edits().is_deleted(1));
        assert!(m.has_change(Loaded(0)));
    });
    assert!(m.update(cx, |m, cx| m.revert_row(Loaded(1), cx)));
    m.read_with(cx, |m, _| {
        assert!(!m.edits().is_deleted(1));
        assert!(!m.has_change(Loaded(1)));
        assert_eq!(m.edits().len(), 1);
    });
    assert!(!m.update(cx, |m, cx| m.revert_row(Loaded(1), cx)));
}

#[gpui_kit::test]
fn a_locator_cell_is_never_set(cx: &mut TestAppContext) {
    let fx = fixture(
        "CREATE TABLE plain(a, b); INSERT INTO plain VALUES (1, 2);",
        "",
    );
    let m = ready(cx, &fx, "plain");
    assert!(!m.update(cx, |m, cx| m.set_value(Loaded(0), 0, EditValue::Null, cx)));
    m.read_with(cx, |m, _| assert_eq!(m.settable(Loaded(0), 0), None));
}

#[gpui_kit::test]
fn a_read_only_browse_refuses_row_operations(cx: &mut TestAppContext) {
    let fx = fixture(T, "read_only = true");
    let m = browsing(cx, &fx, "t", Default::default(), "");
    wait_real(cx, "the read-only browse", |cx| {
        m.read_with(cx, |m, _| m.edit_block().is_some() && !m.running())
    });
    m.update(cx, |m, cx| {
        assert_eq!(m.add_row(cx), None);
        assert_eq!(m.duplicate_row(Loaded(0), cx), None);
        assert!(!m.set_value(Loaded(0), 1, EditValue::Null, cx));
    });
}
