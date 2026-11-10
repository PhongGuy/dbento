//! After a rejected save: drop the conflicting rows and save the rest, or reload them to see
//! what the database holds now.
use crate::support::{
    Fx, Opened, click_id, exec, fixture, init_tokio, label_of, launched, pg_ready, present, ready,
    value_of, wait_idle, wait_prompt, wait_real, wait_unlocked,
};
use dbento_app::results_pane::ResultsPane;
use dbento_app::staged::{Fresh, GridRow, fresh_text, was_fresh_text};
use dbento_app::tab_model::TabModel;
use dbento_core::{CellRef, EditValue, RelationKind};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, ElementId, Entity, Focusable as _, TestAppContext};

const T: &str = "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT); \
                 INSERT INTO t VALUES (1, 'b'), (2, 'a'), (3, 'c');";

/// Rows 0 and 1 (ids 1, 2) staged, id 1 deleted elsewhere, and the save rejected.
fn rejected(cx: &mut TestAppContext, fx: &Fx) -> Entity<TabModel> {
    let m = ready(cx, fx, "t");
    assert!(
        m.update(cx, |m, cx| m.stage_cell(0, 1, "z", cx)),
        "row 0 stages"
    );
    assert!(
        m.update(cx, |m, cx| m.stage_cell(1, 1, "y", cx)),
        "row 1 stages"
    );
    exec(fx, "DELETE FROM t WHERE id = 1");
    assert!(m.update(cx, |m, cx| m.save(cx)), "the save starts");
    wait_idle(cx, &m);
    assert!(
        m.read_with(cx, |m, _| m.edit_banner().is_some()),
        "the conflict shows a banner"
    );
    m
}

#[test]
fn fresh_lines_read_as_the_spec_says() {
    assert_eq!(
        fresh_text(Fresh::Value(CellRef::Text("q"))),
        "now in database: q"
    );
    assert_eq!(fresh_text(Fresh::Gone), "now in database: row deleted");
    assert_eq!(
        was_fresh_text(CellRef::Text("b"), Some(Fresh::Value(CellRef::Null))),
        "Was: b \u{b7} now in database: NULL"
    );
    assert_eq!(was_fresh_text(CellRef::Text("b"), None), "Was: b");
}

#[gpui_kit::test]
fn the_conflicting_rows_are_listed(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = rejected(cx, &fx);
    m.read_with(cx, |m, _| {
        assert_eq!(m.conflicts(), [GridRow::Loaded(0)]);
        assert!(m.can_reload_conflicts());
    });
}

#[gpui_kit::test]
fn dropping_the_conflicts_saves_the_rest_through_the_prompt(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = rejected(cx, &fx);
    assert!(m.update(cx, |m, cx| m.drop_conflicts(cx)));
    m.read_with(cx, |m, _| {
        assert_eq!(m.edits().len(), 1);
        assert_eq!(m.edits().cell(0, 1), None);
        assert_eq!(m.edits().cell(1, 1), Some(&EditValue::Text("y".into())));
        assert!(m.conflicts().is_empty());
    });
    wait_prompt(cx, &m);
    assert_eq!(
        m.read_with(cx, |m, _| m.apply_prompt().unwrap().detail.clone()),
        "Affects 1 row. Locks are held until you choose."
    );
    m.update(cx, |m, cx| m.commit(cx));
    wait_idle(cx, &m);
    assert_eq!(value_of(&fx, "t", "name", 2), "y");
}

#[gpui_kit::test]
fn dropping_is_one_undo_step(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = rejected(cx, &fx);
    assert!(m.update(cx, |m, cx| m.drop_conflicts(cx)));
    wait_prompt(cx, &m);
    m.update(cx, |m, cx| m.rollback(cx));
    wait_idle(cx, &m);
    assert!(m.update(cx, |m, cx| m.undo(cx)));
    m.read_with(cx, |m, _| {
        assert_eq!(m.edits().len(), 2);
        assert_eq!(m.edits().cell(0, 1), Some(&EditValue::Text("z".into())));
    });
}

#[gpui_kit::test]
fn reloading_shows_what_the_database_holds_now(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = ready(cx, &fx, "t");
    for (row, v) in [(0, "z"), (1, "y"), (2, "x")] {
        assert!(m.update(cx, |m, cx| m.stage_cell(row, 1, v, cx)));
    }
    exec(&fx, "DELETE FROM t WHERE id IN (1, 2)");
    assert!(m.update(cx, |m, cx| m.save(cx)));
    wait_idle(cx, &m);
    assert_eq!(
        m.read_with(cx, |m, _| m.conflicts().to_vec()),
        [GridRow::Loaded(0), GridRow::Loaded(1)]
    );
    wait_unlocked(cx, &fx);
    exec(&fx, "INSERT INTO t VALUES (1, 'q')");
    assert!(m.update(cx, |m, cx| m.reload_conflicts(cx)));
    wait_real(cx, "the reload", |cx| {
        m.read_with(cx, |m, _| {
            !m.reloading() && m.fresh_at(GridRow::Loaded(1), 1).is_some()
        })
    });
    m.read_with(cx, |m, _| {
        assert_eq!(
            m.fresh_at(GridRow::Loaded(0), 1),
            Some(Fresh::Value(CellRef::Text("q")))
        );
        assert_eq!(m.fresh_at(GridRow::Loaded(1), 1), Some(Fresh::Gone));
        assert_eq!(m.fresh_at(GridRow::Loaded(2), 1), None);
        assert_eq!(m.edits().len(), 3, "staged edits stay");
    });
}

#[gpui_kit::test]
fn reloading_on_postgres_shows_what_the_database_holds_now(cx: &mut TestAppContext) {
    init_tokio(cx);
    let rt = cx.update(|cx| dbento_app::tokio_bridge::handle(cx));
    let _enter = rt.enter();
    let setup = "CREATE TABLE t(id int PRIMARY KEY, name text); \
                 INSERT INTO t VALUES (1, 'b'), (2, 'a'), (3, 'c');";
    let (db, _dir, m) = pg_ready(cx, &rt, setup, "t");
    let admin = |sql: &'static str| {
        rt.block_on(async { db.admin().await.batch_execute(sql).await.unwrap() });
    };
    for (row, v) in [(0, "z"), (1, "y"), (2, "x")] {
        assert!(m.update(cx, |m, cx| m.stage_cell(row, 1, v, cx)));
    }
    admin("DELETE FROM t WHERE id IN (1, 2)");
    assert!(m.update(cx, |m, cx| m.save(cx)));
    wait_idle(cx, &m);
    assert_eq!(
        m.read_with(cx, |m, _| m.conflicts().to_vec()),
        [GridRow::Loaded(0), GridRow::Loaded(1)]
    );
    admin("INSERT INTO t VALUES (1, 'q')");
    assert!(m.update(cx, |m, cx| m.reload_conflicts(cx)));
    wait_real(cx, "the reload", |cx| {
        m.read_with(cx, |m, _| {
            !m.reloading() && m.fresh_at(GridRow::Loaded(1), 1).is_some()
        })
    });
    m.read_with(cx, |m, _| {
        assert_eq!(
            m.fresh_at(GridRow::Loaded(0), 1),
            Some(Fresh::Value(CellRef::Text("q")))
        );
        assert_eq!(m.fresh_at(GridRow::Loaded(1), 1), Some(Fresh::Gone));
        assert_eq!(m.fresh_at(GridRow::Loaded(2), 1), None);
        assert_eq!(m.edits().len(), 3, "staged edits stay");
    });
}

fn pane(cx: &mut TestAppContext, o: &Opened) -> Entity<ResultsPane> {
    o.shell
        .read_with(cx, |s, cx| s.tab().unwrap().read(cx).results_pane().clone())
}
fn select(cx: &mut TestAppContext, o: &Opened, row: usize, data: usize) {
    let table = pane(cx, o).read_with(cx, |p, _| p.table().clone());
    cx.update_window(o.win, |_, w, cx| {
        table.update(cx, |t, cx| t.set_selected_cell(row, data + 1, cx));
        table.read(cx).focus_handle(cx).focus(w, cx);
    })
    .unwrap();
    cx.run_until_parked();
}
fn click(cx: &mut TestAppContext, o: &Opened, id: impl Into<ElementId>) {
    let id = id.into();
    cx.update_window(o.win, |_, w, cx| w.click(id, cx)).unwrap();
    cx.run_until_parked();
}

#[gpui_kit::test]
fn the_banner_offers_both_ways_forward(cx: &mut TestAppContext) {
    let o = launched(cx, T, "");
    o.open_relation(cx, "t", RelationKind::Table);
    o.wait_browse(cx, "SELECT *\nFROM main.t\nORDER BY id ASC");
    let m = o.model(cx);
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 1, "z", cx)));
    assert!(m.update(cx, |m, cx| m.stage_cell(1, 1, "y", cx)));
    exec(&o.fx, "DELETE FROM t WHERE id = 1");
    assert!(m.update(cx, |m, cx| m.save(cx)));
    wait_idle(cx, &m);
    assert_eq!(
        label_of(cx, o.win, "edit-banner-drop").as_deref(),
        Some("Save the rest, drop conflicting rows")
    );
    assert_eq!(
        label_of(cx, o.win, "edit-banner-reload").as_deref(),
        Some("Reload these rows")
    );
    wait_unlocked(cx, &o.fx);
    exec(&o.fx, "INSERT INTO t VALUES (1, 'q')");
    click(cx, &o, "edit-banner-reload");
    select(cx, &o, 0, 1);
    wait_real(cx, "the fresh value", |cx| {
        label_of(cx, o.win, "status-was").as_deref() == Some("Was: b \u{b7} now in database: q")
    });
    click_id(cx, o.win, "inspector-toggle");
    assert_eq!(
        label_of(cx, o.win, ("inspector-fresh", 1usize)).as_deref(),
        Some("now in database: q")
    );
    click(cx, &o, "edit-banner-drop");
    wait_real(cx, "the prompt", |cx| present(cx, o.win, "apply-commit"));
}

#[gpui_kit::test]
fn reverting_every_conflict_leaves_nothing_to_reload(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = rejected(cx, &fx);
    assert!(m.update(cx, |m, cx| m.revert_row(GridRow::Loaded(0), cx)));
    m.read_with(cx, |m, _| {
        assert!(m.conflicts().is_empty());
        assert!(!m.can_reload_conflicts());
        assert_eq!(m.edit_banner(), None, "no rejected row is left staged");
    });
    assert!(!m.update(cx, |m, cx| m.reload_conflicts(cx)));
    let m = rejected(cx, &fixture(T, ""));
    assert!(m.update(cx, |m, cx| m.revert_cell(GridRow::Loaded(0), 1, cx)));
    m.read_with(cx, |m, _| {
        assert!(m.conflicts().is_empty());
        assert!(!m.can_reload_conflicts());
    });
}

#[gpui_kit::test]
fn a_later_reload_clears_a_failed_reloads_notice(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = rejected(cx, &fx);
    wait_unlocked(cx, &fx);
    exec(&fx, "ALTER TABLE t RENAME TO t2");
    assert!(m.update(cx, |m, cx| m.reload_conflicts(cx)));
    wait_real(cx, "the failed reload", |cx| {
        m.read_with(cx, |m, _| !m.reloading())
    });
    assert!(m.read_with(cx, |m, _| {
        m.notice()
            .is_some_and(|n| n.starts_with("Could not reload"))
    }));
    exec(&fx, "ALTER TABLE t2 RENAME TO t");
    assert!(m.update(cx, |m, cx| m.reload_conflicts(cx)));
    wait_real(cx, "the reload", |cx| {
        m.read_with(cx, |m, _| !m.reloading())
    });
    m.read_with(cx, |m, _| {
        assert_eq!(m.notice(), None);
        assert_eq!(m.fresh_at(GridRow::Loaded(0), 1), Some(Fresh::Gone));
    });
}

#[gpui_kit::test]
fn dropping_when_every_row_conflicts_says_nothing_is_left(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = ready(cx, &fx, "t");
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 1, "z", cx)));
    exec(&fx, "DELETE FROM t WHERE id = 1");
    assert!(m.update(cx, |m, cx| m.save(cx)));
    wait_idle(cx, &m);
    assert!(!m.update(cx, |m, cx| m.drop_conflicts(cx)));
    m.read_with(cx, |m, _| {
        assert_eq!(m.notice(), Some("Nothing left to save"));
        assert_eq!(m.edits().len(), 1, "the edit stays staged");
        assert!(m.edit_banner().is_some());
        assert_eq!(m.conflicts(), [GridRow::Loaded(0)]);
    });
}

#[gpui_kit::test]
fn a_conflicting_delete_says_its_row_is_gone(cx: &mut TestAppContext) {
    let o = launched(cx, T, "");
    o.open_relation(cx, "t", RelationKind::Table);
    o.wait_browse(cx, "SELECT *\nFROM main.t\nORDER BY id ASC");
    let m = o.model(cx);
    assert!(m.update(cx, |m, cx| m.delete_row(0, cx)));
    assert!(m.update(cx, |m, cx| m.stage_cell(1, 1, "y", cx)));
    exec(&o.fx, "DELETE FROM t WHERE id = 1");
    assert!(m.update(cx, |m, cx| m.save(cx)));
    wait_idle(cx, &m);
    assert_eq!(
        m.read_with(cx, |m, _| m.conflicts().to_vec()),
        [GridRow::Loaded(0)]
    );
    wait_unlocked(cx, &o.fx);
    click(cx, &o, "edit-banner-reload");
    wait_real(cx, "the reload", |cx| {
        m.read_with(cx, |m, _| {
            !m.reloading() && m.fresh_at(GridRow::Loaded(0), 0).is_some()
        })
    });
    select(cx, &o, 0, 1);
    assert_eq!(
        label_of(cx, o.win, "status-was").as_deref(),
        Some("now in database: row deleted")
    );
    click_id(cx, o.win, "inspector-toggle");
    assert_eq!(
        label_of(cx, o.win, "inspector-fresh-row").as_deref(),
        Some("now in database: row deleted")
    );
    click_id(cx, o.win, "inspector-toggle");
    click(cx, &o, "row-view-expand");
    assert_eq!(
        label_of(cx, o.win, "record-fresh-row").as_deref(),
        Some("now in database: row deleted")
    );
}
