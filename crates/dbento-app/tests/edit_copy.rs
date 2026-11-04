//! Copy as INSERT, UPDATE and DELETE from a browse tab's row.
mod support;
use dbento_app::browse::EditBlock;
use dbento_app::staged::GridRow::{Inserted, Loaded};
use dbento_app::tab_model::CopyKind;
use dbento_core::{Change, EditBatch, EditValue, RowKey, render_sql};
use gpui_kit::TestAppContext;
use support::{Fx, browsing, edit_target, exec, fixture, ready, wait_real};

const T: &str = "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT, note TEXT, \
                 twice TEXT GENERATED ALWAYS AS (name || name) VIRTUAL); \
                 INSERT INTO t(id, name, note) VALUES (1, 'b', NULL), (2, 'it''s', 'y');";

fn text(s: &str) -> EditValue {
    EditValue::Text(s.into())
}
fn key(id: &str) -> RowKey {
    RowKey::new(vec![("id".into(), text(id))])
}
fn sql(fx: &Fx, table: &str, change: Change) -> String {
    render_sql(&EditBatch::new(edit_target(fx, table), vec![change]).unwrap())
}

#[gpui_kit::test]
fn each_kind_renders_the_row_through_render_sql(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = ready(cx, &fx, "t");
    m.read_with(cx, |m, _| {
        assert!(m.browse_result());
        assert_eq!(
            m.copy_query(Loaded(1), CopyKind::Insert),
            Some(sql(
                &fx,
                "t",
                Change::Insert {
                    values: vec![
                        ("id".into(), text("2")),
                        ("name".into(), text("it's")),
                        ("note".into(), text("y")),
                    ],
                },
            ))
        );
        assert_eq!(
            m.copy_query(Loaded(0), CopyKind::Update),
            Some(sql(
                &fx,
                "t",
                Change::Update {
                    key: key("1"),
                    set: vec![
                        ("id".into(), text("1")),
                        ("name".into(), text("b")),
                        ("note".into(), EditValue::Null),
                    ],
                },
            ))
        );
        assert_eq!(
            m.copy_query(Loaded(0), CopyKind::Delete),
            Some(sql(&fx, "t", Change::Delete { key: key("1") }))
        );
        assert!(
            !m.copy_query(Loaded(0), CopyKind::Insert)
                .unwrap()
                .contains("twice"),
            "a generated column is never written"
        );
    });
}

#[gpui_kit::test]
fn the_copy_shows_staged_values_keyed_by_the_loaded_identity(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = ready(cx, &fx, "t");
    m.update(cx, |m, cx| {
        assert!(m.stage_cell(0, 0, "9", cx));
        assert!(m.stage_cell(0, 1, "z", cx));
        assert_eq!(m.add_row(cx), Some(0));
        assert!(m.stage_text(Inserted(0), 1, "n", cx));
    });
    m.read_with(cx, |m, _| {
        assert_eq!(
            m.copy_query(Loaded(0), CopyKind::Update),
            Some(sql(
                &fx,
                "t",
                Change::Update {
                    key: key("1"),
                    set: vec![
                        ("id".into(), text("9")),
                        ("name".into(), text("z")),
                        ("note".into(), EditValue::Null),
                    ],
                },
            ))
        );
        assert_eq!(
            m.copy_query(Inserted(0), CopyKind::Insert),
            Some(sql(
                &fx,
                "t",
                Change::Insert {
                    values: vec![("name".into(), text("n"))],
                },
            ))
        );
        assert_eq!(m.copy_query(Inserted(0), CopyKind::Update), None);
        assert_eq!(m.copy_query(Inserted(0), CopyKind::Delete), None);
    });
}

#[gpui_kit::test]
fn a_copied_delete_runs_as_written(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = ready(cx, &fx, "t");
    let delete = m
        .read_with(cx, |m, _| m.copy_query(Loaded(1), CopyKind::Delete))
        .unwrap();
    exec(&fx, &delete);
    assert_eq!(fx.count("SELECT count(*) FROM t"), 1);
    assert_eq!(fx.count("SELECT count(*) FROM t WHERE id = 2"), 0);
}

#[gpui_kit::test]
fn a_read_only_connection_still_copies(cx: &mut TestAppContext) {
    let fx = fixture(T, "read_only = true");
    let m = browsing(cx, &fx, "t", Default::default(), "");
    wait_real(cx, "the browse", |cx| {
        m.read_with(cx, |m, _| m.edit_block().is_some() && !m.running())
    });
    m.read_with(cx, |m, _| {
        assert!(m.browse_result());
        assert!(!m.editable());
        assert_eq!(
            m.copy_query(Loaded(0), CopyKind::Delete),
            Some(sql(&fx, "t", Change::Delete { key: key("1") }))
        );
    });
}

#[gpui_kit::test]
fn without_an_identity_only_insert_copies(cx: &mut TestAppContext) {
    let fx = fixture(
        "CREATE TABLE s(rowid, _rowid_, oid); INSERT INTO s VALUES (1, 2, 3);",
        "",
    );
    let m = browsing(cx, &fx, "s", Default::default(), "");
    wait_real(cx, "the browse", |cx| {
        m.read_with(cx, |m, _| m.edit_block().is_some() && !m.running())
    });
    m.read_with(cx, |m, _| {
        assert_eq!(m.edit_block(), Some(EditBlock::NoIdentity));
        assert!(
            m.copy_query(Loaded(0), CopyKind::Insert)
                .is_some_and(|s| s.starts_with("INSERT INTO"))
        );
        assert_eq!(m.copy_query(Loaded(0), CopyKind::Update), None);
        assert_eq!(m.copy_query(Loaded(0), CopyKind::Delete), None);
    });
}

#[gpui_kit::test]
fn a_free_query_copies_nothing(cx: &mut TestAppContext) {
    let o = support::opened(cx, T, "", 0);
    o.run(cx, "SELECT * FROM t");
    o.idle(cx);
    let m = o.model(cx);
    m.read_with(cx, |m, _| {
        assert!(!m.browse_result());
        assert_eq!(m.copy_query(Loaded(0), CopyKind::Insert), None);
    });
}
