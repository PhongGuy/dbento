//! Undo and redo of a browse tab's staged edits, and saving inserted rows.
use crate::support::{browsing, fixture, ready, wait_idle, wait_prompt, wait_real};
use dbento_app::staged::GridRow::{Inserted, Loaded};
use dbento_core::EditValue;
use gpui_kit::TestAppContext;

const T: &str = "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT NOT NULL DEFAULT 'anon', \
                 note TEXT, pic BLOB); \
                 INSERT INTO t VALUES (1, 'b', 'x', x'01'), (2, 'a', NULL, NULL);";

fn text(s: &str) -> EditValue {
    EditValue::Text(s.into())
}

#[gpui_kit::test]
fn undo_and_redo_step_through_the_edits(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = ready(cx, &fx, "t");
    m.update(cx, |m, cx| {
        assert!(!m.can_undo());
        assert!(m.stage_cell(0, 1, "z", cx));
        assert!(
            m.stage_cell(0, 1, "z", cx),
            "the same value again is no new step"
        );
        assert_eq!(m.add_row(cx), Some(0));
        m.revert_edits(cx);
        assert!(m.edits().is_empty());
        assert!(m.undo(cx), "Revert all is undoable");
        assert_eq!(m.edits().len(), 2);
        assert!(m.undo(cx));
        assert_eq!(m.edits().inserted(), 0);
        assert!(m.undo(cx));
        assert!(m.edits().is_empty());
        assert!(!m.undo(cx));
        assert!(m.redo(cx));
        assert_eq!(m.edits().cell(0, 1), Some(&text("z")));
        assert!(m.can_redo());
        assert!(m.delete_row(1, cx));
        assert!(!m.can_redo(), "a new edit drops the redo steps");
    });
}

#[gpui_kit::test]
fn a_new_run_forgets_the_history(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = ready(cx, &fx, "t");
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 1, "z", cx)));
    m.update(cx, |m, cx| m.run("SELECT 1".into(), cx));
    m.read_with(cx, |m, _| assert!(!m.can_undo() && !m.can_redo()));
}

#[gpui_kit::test]
fn an_inserted_row_is_saved_and_committed(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = ready(cx, &fx, "t");
    m.update(cx, |m, cx| {
        assert_eq!(m.add_row(cx), Some(0));
        assert!(m.stage_text(Inserted(0), 2, "added", cx));
    });
    assert!(m.update(cx, |m, cx| m.save(cx)));
    wait_prompt(cx, &m);
    assert!(
        !m.update(cx, |m, cx| m.undo(cx)),
        "no undo while the transaction waits"
    );
    m.update(cx, |m, cx| m.commit(cx));
    wait_real(cx, "the commit and re-run", |cx| {
        m.read_with(cx, |m, _| {
            m.edits().is_empty()
                && !m.running()
                && m.results().first().is_some_and(|r| r.store.rows() == 3)
        })
    });
    wait_idle(cx, &m);
    assert_eq!(
        fx.count("SELECT count(*) FROM t WHERE note = 'added' AND name = 'anon'"),
        1
    );
    m.read_with(cx, |m, _| assert!(!m.can_undo()));
}

#[gpui_kit::test]
fn a_refused_insert_is_marked_and_kept(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = ready(cx, &fx, "t");
    m.update(cx, |m, cx| {
        assert_eq!(m.add_row(cx), Some(0));
        assert!(
            m.stage_text(Inserted(0), 0, "1", cx),
            "a primary key that exists"
        );
    });
    assert!(m.update(cx, |m, cx| m.save(cx)));
    wait_real(cx, "the rejection", |cx| {
        m.read_with(cx, |m, _| m.edit_banner().is_some())
    });
    wait_idle(cx, &m);
    m.read_with(cx, |m, _| {
        let mark = m.edits().new_mark(0).map(str::to_owned);
        assert!(
            mark.as_deref().is_some_and(|t| t.contains("UNIQUE")),
            "{mark:?}"
        );
        assert_eq!(m.edits().inserted(), 1);
    });
}

#[gpui_kit::test]
fn a_read_only_browse_has_nothing_to_undo(cx: &mut TestAppContext) {
    let fx = fixture(T, "read_only = true");
    let m = browsing(cx, &fx, "t", Default::default(), "");
    wait_real(cx, "the read-only browse", |cx| {
        m.read_with(cx, |m, _| m.edit_block().is_some() && !m.running())
    });
    m.update(cx, |m, cx| {
        assert!(!m.set_value(Loaded(0), 1, EditValue::Null, cx));
        assert!(!m.undo(cx));
        assert!(!m.can_redo());
    });
}

#[gpui_kit::test]
fn a_later_save_drops_stale_marks_from_the_history(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = ready(cx, &fx, "t");
    m.update(cx, |m, cx| {
        assert_eq!(m.add_row(cx), Some(0));
        assert!(
            m.stage_text(Inserted(0), 0, "1", cx),
            "a primary key that exists"
        );
    });
    assert!(m.update(cx, |m, cx| m.save(cx)));
    wait_real(cx, "the rejection", |cx| {
        m.read_with(cx, |m, _| m.edit_banner().is_some())
    });
    wait_idle(cx, &m);
    m.update(cx, |m, cx| {
        assert!(m.edits().new_mark(0).is_some());
        assert!(m.stage_text(Inserted(0), 0, "5", cx));
        assert!(m.stage_text(Inserted(0), 2, "n", cx));
        assert!(m.undo(cx));
        assert!(m.can_redo());
    });
    assert!(m.update(cx, |m, cx| m.save(cx)));
    wait_prompt(cx, &m);
    m.update(cx, |m, cx| {
        assert!(
            !m.can_redo() && !m.redo(cx),
            "no redo while the transaction waits"
        );
        m.rollback(cx);
    });
    wait_idle(cx, &m);
    m.update(cx, |m, cx| {
        assert!(m.redo(cx));
        assert_eq!(m.edits().new_mark(0), None, "the redo step's mark is stale");
        assert!(m.undo(cx) && m.undo(cx));
        assert_eq!(m.edits().new_cell(0, 0), Some(&text("1")));
        assert_eq!(m.edits().new_mark(0), None, "the undo step's mark is stale");
    });
}
