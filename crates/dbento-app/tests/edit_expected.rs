//! A grid save sends the loaded values it staged against, so a row changed elsewhere since
//! it loaded is refused instead of overwritten.
use crate::support::{exec, fixture, ready, value_of, wait_idle, wait_unlocked};
use dbento_app::staged::GridRow;
use dbento_core::{Change, EditValue, RowKey};
use gpui_kit::TestAppContext;

const T: &str = "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT, note TEXT, \
                 twice TEXT GENERATED ALWAYS AS (name || name) VIRTUAL); \
                 INSERT INTO t(id, name, note) VALUES (1, 'b', NULL), (2, 'a', 'y');";

fn text(s: &str) -> EditValue {
    EditValue::Text(s.into())
}
fn key(id: &str) -> RowKey {
    RowKey::new(vec![("id".into(), text(id))])
}

#[gpui_kit::test]
fn a_staged_cell_expects_its_loaded_value(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = ready(cx, &fx, "t");
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 1, "mine", cx)));
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 2, "n", cx)));
    assert_eq!(
        m.read_with(cx, |m, _| m.edits().changes().0),
        vec![Change::Update {
            key: key("1"),
            set: vec![("name".into(), text("mine")), ("note".into(), text("n"))],
            expected: vec![("name".into(), text("b")), ("note".into(), EditValue::Null)],
        }]
    );
}

#[gpui_kit::test]
fn a_deleted_row_expects_every_loaded_column_of_the_target(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = ready(cx, &fx, "t");
    assert!(m.update(cx, |m, cx| m.stage_cell(1, 1, "q", cx)));
    assert!(m.update(cx, |m, cx| m.delete_row(1, cx)));
    assert_eq!(
        m.read_with(cx, |m, _| m.edits().changes().0),
        vec![Change::Delete {
            key: key("2"),
            expected: vec![
                ("id".into(), text("2")),
                ("name".into(), text("a")),
                ("note".into(), text("y")),
                ("twice".into(), text("aa")),
            ],
        }]
    );
}

#[gpui_kit::test]
fn an_update_over_a_value_changed_elsewhere_conflicts_and_keeps_theirs(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = ready(cx, &fx, "t");
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 1, "mine", cx)));
    exec(&fx, "UPDATE t SET name = 'theirs' WHERE id = 1");
    assert!(m.update(cx, |m, cx| m.save(cx)));
    wait_idle(cx, &m);
    m.read_with(cx, |m, _| {
        assert_eq!(m.conflicts(), [GridRow::Loaded(0)]);
        assert!(m.edit_banner().is_some());
        assert!(m.apply_prompt().is_none());
    });
    wait_unlocked(cx, &fx);
    assert_eq!(value_of(&fx, "t", "name", 1), "theirs");
}

#[gpui_kit::test]
fn a_delete_of_a_row_changed_elsewhere_conflicts_and_the_row_survives(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = ready(cx, &fx, "t");
    assert!(m.update(cx, |m, cx| m.delete_row(0, cx)));
    exec(&fx, "UPDATE t SET note = 'late' WHERE id = 1");
    assert!(m.update(cx, |m, cx| m.save(cx)));
    wait_idle(cx, &m);
    assert_eq!(
        m.read_with(cx, |m, _| m.conflicts().to_vec()),
        [GridRow::Loaded(0)]
    );
    wait_unlocked(cx, &fx);
    assert_eq!(fx.count("SELECT count(*) FROM t WHERE id = 1"), 1);
}
