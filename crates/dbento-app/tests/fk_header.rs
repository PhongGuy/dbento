//! Foreign keys arrive with a browse tab's target; PK and FK columns carry header icons.
use crate::support::{fixture, label_of, launched, present, ready, wait_real};
use dbento_core::{FkDirection, RelationKind, RelationRef};
use gpui_kit::TestAppContext;

const T: &str = "CREATE TABLE a(id INTEGER PRIMARY KEY, name TEXT); \
                 CREATE TABLE b(id INTEGER PRIMARY KEY, a_id INTEGER REFERENCES a(id), note TEXT); \
                 INSERT INTO a VALUES (1, 'x'); INSERT INTO b VALUES (1, 1, 'n'), (2, 1, 'm');";

#[gpui_kit::test]
fn an_outgoing_key_survives_a_filter(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = ready(cx, &fx, "b");
    m.read_with(cx, |m, _| {
        let keys = m.browse().unwrap().foreign_keys();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].direction(), FkDirection::Outgoing);
        assert_eq!(keys[0].referenced(), &RelationRef::new("main", "a"));
        assert_eq!(keys[0].columns(), ["a_id"]);
    });
    assert!(m.update(cx, |m, cx| m.apply_filter("id = 1", cx)));
    wait_real(cx, "the filtered browse", |cx| {
        m.read_with(cx, |m, _| {
            m.editable() && !m.running() && m.results()[0].store.rows() == 1
        })
    });
    m.read_with(cx, |m, _| {
        assert_eq!(m.browse().unwrap().foreign_keys().len(), 1)
    });
}

#[gpui_kit::test]
fn the_referenced_table_sees_the_key_incoming(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = ready(cx, &fx, "a");
    m.read_with(cx, |m, _| {
        let keys = m.browse().unwrap().foreign_keys();
        assert_eq!(keys.len(), 1);
        assert_eq!(keys[0].direction(), FkDirection::Incoming);
        assert_eq!(keys[0].table(), &RelationRef::new("main", "b"));
    });
}

#[gpui_kit::test]
fn pk_and_fk_columns_carry_icons_on_the_browse_result_only(cx: &mut TestAppContext) {
    let o = launched(cx, T, "");
    o.open_relation(cx, "b", RelationKind::Table);
    o.wait_browse(cx, "SELECT *\nFROM main.b\nORDER BY id ASC");
    assert!(present(cx, o.win, ("result-pk", 0usize)));
    assert_eq!(
        label_of(cx, o.win, ("result-fk", 1usize)).as_deref(),
        Some("foreign key to a")
    );
    assert!(!present(cx, o.win, ("result-fk", 0usize)));
    assert!(!present(cx, o.win, ("result-pk", 1usize)));
    assert!(!present(cx, o.win, ("result-fk", 2usize)));
    o.run(cx, "SELECT * FROM b");
    o.idle(cx);
    assert!(!present(cx, o.win, ("result-pk", 0usize)));
    assert!(!present(cx, o.win, ("result-fk", 1usize)));
}
