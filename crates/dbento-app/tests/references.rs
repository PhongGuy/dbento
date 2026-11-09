//! Reference links of one browse row: the far table, its key and the filter that browses it.
use crate::support::{fixture, ready, wait_real};
use dbento_app::record::{RefLink, key_text, references};
use dbento_app::staged::GridRow::{Inserted, Loaded};
use dbento_core::{EditValue, FkDirection, RelationRef};
use gpui_kit::TestAppContext;

const T: &str = "CREATE TABLE a(id INTEGER PRIMARY KEY, name TEXT); \
                 CREATE TABLE b(id INTEGER PRIMARY KEY, a_id INTEGER REFERENCES a(id), note TEXT); \
                 INSERT INTO a VALUES (1, 'x'), (2, 'y'); \
                 INSERT INTO b VALUES (1, 1, 'n'), (2, NULL, 'm'), (3, 1, 'o');";

fn text(s: &str) -> EditValue {
    EditValue::Text(s.into())
}

#[test]
fn key_text_joins_columns_and_values() {
    assert_eq!(key_text(&[("id".into(), text("1"))]), "id = 1");
    assert_eq!(
        key_text(&[
            ("a".into(), EditValue::Null),
            ("b".into(), EditValue::Bytes(vec![0xab]))
        ]),
        "a = NULL, b = \\xab"
    );
}

#[gpui_kit::test]
fn an_outgoing_link_keys_the_referenced_table_by_the_loaded_value(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = ready(cx, &fx, "b");
    m.read_with(cx, |m, _| {
        assert_eq!(
            references(m, Loaded(0)),
            vec![RefLink {
                direction: FkDirection::Outgoing,
                relation: RelationRef::new("main", "a"),
                key: vec![("id".to_owned(), text("1"))],
                filter: Some("id = '1'".to_owned()),
            }]
        );
        let null = references(m, Loaded(1));
        assert_eq!(null[0].filter, None);
        assert_eq!(key_text(&null[0].key), "id = NULL");
    });
    assert!(m.update(cx, |m, cx| m.set_value(Loaded(0), 1, text("2"), cx)));
    m.read_with(cx, |m, _| {
        assert_eq!(
            references(m, Loaded(0))[0].filter.as_deref(),
            Some("id = '1'")
        );
    });
    assert_eq!(m.update(cx, |m, cx| m.add_row(cx)), Some(0));
    m.read_with(cx, |m, _| assert!(references(m, Inserted(0)).is_empty()));
}

#[gpui_kit::test]
fn an_incoming_link_keys_the_referencing_table_by_this_rows_key(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = ready(cx, &fx, "a");
    m.read_with(cx, |m, _| {
        assert_eq!(
            references(m, Loaded(0)),
            vec![RefLink {
                direction: FkDirection::Incoming,
                relation: RelationRef::new("main", "b"),
                key: vec![("a_id".to_owned(), text("1"))],
                filter: Some("a_id = '1'".to_owned()),
            }]
        );
    });
}

#[gpui_kit::test]
fn a_free_result_has_no_links(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = ready(cx, &fx, "b");
    m.update(cx, |m, cx| m.run("SELECT * FROM b".into(), cx));
    wait_real(cx, "the free result", |cx| {
        m.read_with(cx, |m, _| {
            !m.running() && !m.browse_result() && !m.results().is_empty()
        })
    });
    m.read_with(cx, |m, _| assert!(references(m, Loaded(0)).is_empty()));
}

// A composite key pairs each local column with its own far column, whatever their order.
#[gpui_kit::test]
fn a_composite_key_pairs_columns_in_key_order(cx: &mut TestAppContext) {
    let fx = fixture(
        "CREATE TABLE p(x INTEGER, y INTEGER, PRIMARY KEY(x, y)); \
         CREATE TABLE c(id INTEGER PRIMARY KEY, a INTEGER, b INTEGER, \
                        FOREIGN KEY(a, b) REFERENCES p(y, x)); \
         INSERT INTO p VALUES (1, 2); INSERT INTO c VALUES (1, 2, 1);",
        "",
    );
    let c = ready(cx, &fx, "c");
    c.read_with(cx, |m, _| {
        let out = &references(m, Loaded(0))[0];
        assert_eq!(out.relation, RelationRef::new("main", "p"));
        assert_eq!(out.filter.as_deref(), Some("y = '2' AND x = '1'"));
    });
    let p = ready(cx, &fx, "p");
    p.read_with(cx, |m, _| {
        let inc = &references(m, Loaded(0))[0];
        assert_eq!(inc.relation, RelationRef::new("main", "c"));
        assert_eq!(inc.filter.as_deref(), Some("a = '2' AND b = '1'"));
    });
}
