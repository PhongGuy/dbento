//! A committed row the browse filter no longer matches stays listed, muted, until the next run.
use crate::support::{
    fixture, init_tokio, label_of, launched, pg_ready, ready, wait_idle, wait_prompt, wait_real,
};
use dbento_app::staged::GridRow;
use dbento_app::staged::{KEPT_SHOWN, kept_label, kept_more};
use dbento_app::tab_model::TabModel;
use dbento_core::{EditValue, RelationKind, RowKey};
use gpui_kit::{Entity, TestAppContext};

const T: &str = "CREATE TABLE t(id INTEGER PRIMARY KEY, status TEXT); \
                 INSERT INTO t VALUES (1, 'pending'), (2, 'pending'), (3, 'pending'), (4, 'done');";

fn filtered(cx: &mut TestAppContext, m: &Entity<TabModel>, filter: &str) {
    assert!(m.update(cx, |m, cx| m.apply_filter(filter, cx)));
    wait_real(cx, "the filtered run", |cx| {
        m.read_with(cx, |m, _| {
            !m.running()
                && m.results()
                    .first()
                    .is_some_and(|r| r.sql.contains("WHERE") && r.store.end().is_some())
        })
    });
}
/// Saves, commits, and waits for the commit's re-run to end or park.
fn commit(cx: &mut TestAppContext, m: &Entity<TabModel>) {
    let before = m.read_with(cx, |m, _| m.run_id());
    assert!(m.update(cx, |m, cx| m.save(cx)));
    wait_prompt(cx, m);
    m.update(cx, |m, cx| m.commit(cx));
    wait_idle(cx, m);
    wait_real(cx, "the re-run", |cx| {
        m.read_with(cx, |m, _| {
            m.run_id() > before
                && (m.parked_at_cap().is_some()
                    || m.results().first().is_some_and(|r| r.store.end().is_some()))
        })
    });
}
fn key(id: &str) -> RowKey {
    RowKey::new(vec![("id".into(), EditValue::Text(id.into()))])
}

#[test]
fn the_label_names_the_key_and_the_change() {
    assert_eq!(
        kept_label(
            &key("1"),
            &[("status".into(), EditValue::Text("shipped".into()))]
        ),
        "id = 1 \u{b7} status: shipped \u{b7} no longer matches filter"
    );
    assert_eq!(
        kept_label(&key("2"), &[("status".into(), EditValue::Null)]),
        "id = 2 \u{b7} status: NULL \u{b7} no longer matches filter"
    );
    assert_eq!(KEPT_SHOWN, 20);
    assert_eq!(kept_more(20), None);
    assert_eq!(kept_more(25).as_deref(), Some("and 5 more"));
}

#[gpui_kit::test]
fn a_committed_row_that_left_the_filter_stays_listed_until_the_next_run(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = ready(cx, &fx, "t");
    filtered(cx, &m, "status = 'pending'");
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 1, "shipped", cx)));
    let ix = m.update(cx, |m, cx| m.add_row(cx)).unwrap();
    assert!(m.update(cx, |m, cx| m.stage_text(
        dbento_app::staged::GridRow::Inserted(ix),
        1,
        "shipped",
        cx
    )));
    commit(cx, &m);
    m.read_with(cx, |m, _| {
        assert_eq!(m.results()[0].store.rows(), 2);
        let kept: Vec<&str> = m.kept_rows().iter().map(|k| k.label.as_str()).collect();
        assert_eq!(
            kept,
            ["id = 1 \u{b7} status: shipped \u{b7} no longer matches filter"],
            "an inserted row is not kept"
        );
    });
    let sql = m.read_with(cx, |m, _| m.browse().unwrap().sql().to_owned());
    m.update(cx, |m, cx| m.run(sql, cx));
    wait_real(cx, "the user's run", |cx| {
        m.read_with(cx, |m, _| {
            m.results().first().is_some_and(|r| r.store.end().is_some())
        })
    });
    assert!(m.read_with(cx, |m, _| m.kept_rows().is_empty()));
}

#[gpui_kit::test]
fn a_row_that_still_matches_is_not_kept(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = ready(cx, &fx, "t");
    filtered(cx, &m, "status LIKE 'p%'");
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 1, "paid", cx)));
    commit(cx, &m);
    assert!(m.read_with(cx, |m, _| m.kept_rows().is_empty()));
}

#[gpui_kit::test]
fn a_truncated_rerun_keeps_nothing(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = ready(cx, &fx, "t");
    filtered(cx, &m, "status = 'pending'");
    m.update(cx, |m, _| m.set_max_rows(Some(1)));
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 1, "shipped", cx)));
    commit(cx, &m);
    assert!(m.read_with(cx, |m, _| m.kept_rows().is_empty()));
}

#[gpui_kit::test]
fn the_band_lists_the_kept_rows(cx: &mut TestAppContext) {
    let o = launched(cx, T, "");
    o.open_relation(cx, "t", RelationKind::Table);
    o.wait_browse(cx, "SELECT *\nFROM main.t\nORDER BY id ASC");
    let m = o.model(cx);
    filtered(cx, &m, "status = 'pending'");
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 1, "shipped", cx)));
    commit(cx, &m);
    assert_eq!(
        label_of(cx, o.win, ("kept-row", 0usize)).as_deref(),
        Some("id = 1 \u{b7} status: shipped \u{b7} no longer matches filter")
    );
}

#[gpui_kit::test]
fn editing_the_key_of_a_still_matching_row_keeps_nothing(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = ready(cx, &fx, "t");
    filtered(cx, &m, "status = 'pending'");
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 0, "10", cx)));
    commit(cx, &m);
    assert!(m.read_with(cx, |m, _| m.kept_rows().is_empty()));
}

fn kept_labels(cx: &mut TestAppContext, m: &Entity<TabModel>) -> Vec<String> {
    m.read_with(cx, |m, _| {
        m.kept_rows().iter().map(|k| k.label.clone()).collect()
    })
}

#[gpui_kit::test]
fn a_key_typed_in_another_form_matches_its_stored_value(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = ready(cx, &fx, "t");
    filtered(cx, &m, "status = 'pending'");
    // SQLite stores "010" under INTEGER affinity as 10, which the re-run loads.
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 0, "010", cx)));
    commit(cx, &m);
    assert_eq!(kept_labels(cx, &m), Vec::<String>::new());
}

/// Not a rowid alias, so NULL is allowed in an UPDATE and the REPLACE writes the default.
const NULLED: &str = "CREATE TABLE n(id INT PRIMARY KEY NOT NULL ON CONFLICT REPLACE DEFAULT 50, \
                      status TEXT); INSERT INTO n VALUES (1, 'pending'), (2, 'done');";

#[gpui_kit::test]
fn a_key_set_to_null_matches_the_value_the_database_wrote(cx: &mut TestAppContext) {
    let fx = fixture(NULLED, "");
    let m = ready(cx, &fx, "n");
    filtered(cx, &m, "status = 'pending'");
    assert!(m.update(cx, |m, cx| m.set_value(
        GridRow::Loaded(0),
        0,
        EditValue::Null,
        cx
    )));
    commit(cx, &m);
    assert_eq!(fx.count("SELECT count(*) FROM n WHERE id = 50"), 1);
    assert_eq!(kept_labels(cx, &m), Vec::<String>::new());
}

#[gpui_kit::test]
fn a_row_that_left_the_filter_under_a_new_key_is_still_kept(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = ready(cx, &fx, "t");
    filtered(cx, &m, "status = 'pending'");
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 0, "011", cx)));
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 1, "shipped", cx)));
    commit(cx, &m);
    assert_eq!(kept_labels(cx, &m).len(), 1);
}

const PG: &str = "CREATE TABLE t(id int PRIMARY KEY DEFAULT 100, status text); \
                  INSERT INTO t VALUES (1, 'pending'), (2, 'pending'), (3, 'pending'), (4, 'done');";

#[gpui_kit::test]
fn postgres_keys_typed_in_another_form_or_set_to_default_match(cx: &mut TestAppContext) {
    init_tokio(cx);
    let rt = cx.update(|cx| dbento_app::tokio_bridge::handle(cx));
    let _enter = rt.enter();
    let (_db, _dir, m) = pg_ready(cx, &rt, PG, "t");
    filtered(cx, &m, "status = 'pending'");
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 0, "010", cx)));
    assert!(m.update(cx, |m, cx| m.set_value(
        GridRow::Loaded(1),
        0,
        EditValue::Default,
        cx
    )));
    commit(cx, &m);
    let ids: Vec<String> = m.read_with(cx, |m, _| {
        let store = &m.results()[0].store;
        (0..store.rows())
            .map(|r| format!("{:?}", store.cell(r, 0)))
            .collect()
    });
    assert_eq!(ids.len(), 3, "{ids:?}");
    assert_eq!(kept_labels(cx, &m), Vec::<String>::new());
}
