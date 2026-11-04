//! Saving a browse tab's staged edits: the open transaction, commit and its re-run, roll
//! back, and rejected changes marked and kept.
mod support;
use dbento_app::staged::RowEdit;
use dbento_app::tab_model::{SaveState, TabModel};
use dbento_core::{CellRef, Change, EditBatch, EditValue, RowKey, render_sql};
use gpui_kit::{Entity, TestAppContext};
use support::{edit_target, exec, fixture, ready, value_of, wait_idle, wait_prompt, wait_real};

const T: &str = "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT); \
                 INSERT INTO t VALUES (1, 'b'), (2, 'a'), (3, 'c');";

fn cell(cx: &mut TestAppContext, m: &Entity<TabModel>, row: u64, col: usize) -> Option<String> {
    m.read_with(cx, |m, _| {
        match m.results().first()?.store.cell(row, col)? {
            CellRef::Text(s) => Some(s.to_owned()),
            _ => None,
        }
    })
}

#[gpui_kit::test]
fn commit_writes_the_edit_and_runs_the_browse_again(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = ready(cx, &fx, "t");
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 1, "z", cx)));
    let first_run = m.read_with(cx, |m, _| m.run_id());
    assert!(m.update(cx, |m, cx| m.save(cx)));
    wait_prompt(cx, &m);
    let batch = EditBatch::new(
        edit_target(&fx, "t"),
        vec![Change::Update {
            key: RowKey::new(vec![("id".into(), EditValue::Text("1".into()))]),
            set: vec![("name".into(), EditValue::Text("z".into()))],
        }],
    )
    .unwrap();
    let prompt = m.read_with(cx, |m, _| m.apply_prompt().cloned()).unwrap();
    assert_eq!(prompt.title, "Apply changes to main.t");
    assert_eq!(
        prompt.detail,
        "Affects 1 row. Locks are held until you choose."
    );
    assert_eq!(prompt.sql, render_sql(&batch));
    assert_eq!(m.read_with(cx, |m, _| m.save_state()), SaveState::Prompt);
    assert_eq!(
        value_of(&fx, "t", "name", 1),
        "b",
        "nothing commits before the choice"
    );
    assert!(
        !m.update(cx, |m, cx| m.stage_cell(1, 1, "q", cx)),
        "no staging while a save is in flight"
    );
    m.update(cx, |m, cx| m.commit(cx));
    wait_real(cx, "the re-run", |cx| {
        m.read_with(cx, |m, _| {
            m.run_id() > first_run && !m.running() && m.save_state() == SaveState::Idle
        })
    });
    assert_eq!(value_of(&fx, "t", "name", 1), "z");
    assert!(m.read_with(cx, |m, _| m.edits().is_empty() && m.edit_banner().is_none()));
    assert_eq!(cell(cx, &m, 0, 1).as_deref(), Some("z"));
}

#[gpui_kit::test]
fn roll_back_leaves_the_table_and_keeps_the_edits(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = ready(cx, &fx, "t");
    assert!(m.update(cx, |m, cx| m.delete_row(1, cx)));
    assert!(matches!(
        m.read_with(cx, |m, _| m.edits().row(1).cloned()),
        Some(RowEdit::Delete { .. })
    ));
    assert!(m.update(cx, |m, cx| m.save(cx)));
    wait_prompt(cx, &m);
    m.update(cx, |m, cx| m.rollback(cx));
    wait_idle(cx, &m);
    assert_eq!(fx.count("SELECT count(*) FROM t"), 3);
    m.read_with(cx, |m, _| {
        assert!(m.apply_prompt().is_none());
        assert!(m.edits().is_deleted(1));
        assert_eq!(m.edit_banner(), None);
    });
}

#[gpui_kit::test]
fn a_row_changed_elsewhere_is_rejected_marked_and_kept(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = ready(cx, &fx, "t");
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 1, "z", cx)));
    exec(&fx, "DELETE FROM t WHERE id = 1");
    assert!(m.update(cx, |m, cx| m.save(cx)));
    wait_idle(cx, &m);
    m.read_with(cx, |m, _| {
        assert!(m.apply_prompt().is_none());
        assert_eq!(
            m.edit_banner(),
            Some("Nothing was committed: 1 row was modified by someone else")
        );
        assert_eq!(
            m.edits().mark(0),
            Some("modified or deleted by someone else")
        );
        assert_eq!(m.edits().len(), 1);
    });
    assert_eq!(fx.count("SELECT count(*) FROM t"), 2);
}

#[gpui_kit::test]
fn a_value_the_database_refuses_fails_with_its_message(cx: &mut TestAppContext) {
    let fx = fixture(
        "CREATE TABLE c(id INTEGER PRIMARY KEY, n INTEGER CHECK (n > 0)); \
         INSERT INTO c VALUES (1, 5);",
        "",
    );
    let m = ready(cx, &fx, "c");
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 1, "-1", cx)));
    assert!(m.update(cx, |m, cx| m.save(cx)));
    wait_idle(cx, &m);
    m.read_with(cx, |m, _| {
        let banner = m.edit_banner().unwrap();
        assert!(
            banner.starts_with("Nothing was committed: CHECK constraint failed"),
            "{banner}"
        );
        assert!(
            m.edits()
                .mark(0)
                .unwrap()
                .contains("CHECK constraint failed")
        );
    });
    assert_eq!(fx.count("SELECT n FROM c WHERE id = 1"), 5);
}

#[gpui_kit::test]
fn nothing_staged_saves_nothing(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = ready(cx, &fx, "t");
    assert!(!m.read_with(cx, |m, _| m.can_save()));
    assert!(!m.update(cx, |m, cx| m.save(cx)));
    assert_eq!(m.read_with(cx, |m, _| m.save_state()), SaveState::Idle);
    assert!(m.read_with(cx, |m, _| m.apply_prompt().is_none()));
}
