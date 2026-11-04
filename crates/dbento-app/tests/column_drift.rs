//! A table whose columns change between a browse and its continuation or conflict reload is
//! never mapped by position: the browse loads fresh, or, with edits staged, saving stops.
mod support;
use std::time::Duration;

use dbento_app::browse::BrowseEvent;
use dbento_app::runner::RunnerTuning;
use dbento_app::staged::GridRow;
use dbento_app::tab_model::{COLUMNS_BLOCKED, COLUMNS_RELOADED, TabModel};
use dbento_core::{EditValue, RelationRef};
use gpui_kit::{AppContext as _, Entity, TestAppContext};
use support::{browsing, exec, fixture, init_tokio, ready, wait_idle, wait_real, wait_unlocked};

const LITE: &str = "CREATE TABLE t(id INTEGER PRIMARY KEY, g INTEGER, v TEXT); \
    WITH RECURSIVE r(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM r WHERE x < 2500) \
    INSERT INTO t SELECT x, x % 7, 'v' || x FROM r;";
const PG: &str = "CREATE TABLE t(id int PRIMARY KEY, g int, v text); \
    INSERT INTO t SELECT x, x % 7, 'v' || x FROM generate_series(1, 2500) x;";
const SMALL: &str = "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT); \
                     INSERT INTO t VALUES (1, 'b'), (2, 'a'), (3, 'c');";

/// No first-rows pause below the cap, and parks that expire after 300 ms.
fn tuning() -> RunnerTuning {
    RunnerTuning {
        first_rows: 100_000,
        park_at_cap: Duration::from_millis(300),
        ..RunnerTuning::default()
    }
}

fn ended(cx: &mut TestAppContext, m: &Entity<TabModel>) {
    wait_real(cx, "result 0 to end", |cx| {
        m.read_with(cx, |m, _| {
            !m.running() && m.results().first().is_some_and(|r| r.store.end().is_some())
        })
    });
}

fn names(cx: &mut TestAppContext, m: &Entity<TabModel>) -> Vec<String> {
    m.read_with(cx, |m, _| {
        m.results()[0]
            .store
            .columns()
            .iter()
            .map(|c| c.name().to_owned())
            .collect()
    })
}

/// Waits for result 0 to end with columns `want`, as a fresh load reads them.
fn loaded_as(cx: &mut TestAppContext, m: &Entity<TabModel>, want: &[&str]) {
    wait_real(cx, "the fresh load", |cx| {
        m.read_with(cx, |m, _| {
            !m.running()
                && m.results().first().is_some_and(|r| {
                    r.store.end().is_some()
                        && r.store
                            .columns()
                            .iter()
                            .map(|c| c.name())
                            .eq(want.iter().copied())
                })
        })
    });
}

/// A browse of `public.t` on a fresh Postgres database built by `setup`. The caller holds
/// `rt.enter()` for the whole test, since a container's drop needs a runtime.
fn pg_browse(
    cx: &mut TestAppContext,
    rt: &tokio::runtime::Handle,
    setup: &str,
    tuning: RunnerTuning,
) -> (dbento_testkit::PgDb, tempfile::TempDir, Entity<TabModel>) {
    let (db, dir, spec) = rt.block_on(support::pg_spec(""));
    rt.block_on(async { db.admin().await.batch_execute(setup).await.unwrap() });
    let m = cx.new(|_| TabModel::with_tuning(spec, tuning));
    cx.update(|cx| {
        cx.subscribe(&m, |m, e: &BrowseEvent, cx| {
            if let BrowseEvent::Run(sql) = e {
                let sql = sql.clone();
                m.update(cx, |m, cx| m.run(sql, cx));
            }
        })
        .detach();
    });
    m.update(cx, |m, cx| {
        m.load_target(RelationRef::new("public", "t"), cx)
    });
    (db, dir, m)
}

fn runtime(cx: &mut TestAppContext) -> tokio::runtime::Handle {
    init_tokio(cx);
    cx.update(|cx| dbento_app::tokio_bridge::handle(cx))
}

#[test]
fn the_notices_read_as_the_issue_asks() {
    assert_eq!(
        COLUMNS_RELOADED,
        "The table's columns changed since it loaded, so it was loaded again."
    );
    assert_eq!(
        COLUMNS_BLOCKED,
        "The table's columns changed since it loaded. Saving is off so no staged edit \
         lands on the wrong column; run the table again to load it fresh."
    );
}

#[gpui_kit::test]
fn a_continuation_after_a_rename_loads_the_table_fresh(cx: &mut TestAppContext) {
    let fx = fixture(LITE, "");
    let m = browsing(cx, &fx, "t", tuning(), "");
    ended(cx, &m);
    exec(&fx, "ALTER TABLE t RENAME COLUMN v TO w");
    assert!(m.update(cx, |m, cx| m.continue_browse(cx)));
    loaded_as(cx, &m, &["id", "g", "w"]);
    m.read_with(cx, |m, _| {
        assert_eq!(
            m.results()[0].store.rows(),
            1000,
            "a first page, not an append"
        );
        assert_eq!(m.notice(), Some(COLUMNS_RELOADED));
    });
}

#[gpui_kit::test]
fn a_continuation_after_a_change_keeps_staged_edits_and_stops_saving(cx: &mut TestAppContext) {
    let fx = fixture(LITE, "");
    let m = browsing(cx, &fx, "t", tuning(), "");
    ended(cx, &m);
    assert!(m.update(cx, |m, cx| m.stage_cell(5, 2, "w", cx)));
    exec(&fx, "ALTER TABLE t ADD COLUMN extra TEXT");
    assert!(m.update(cx, |m, cx| m.continue_browse(cx)));
    ended(cx, &m);
    m.read_with(cx, |m, _| {
        assert_eq!(
            m.results()[0].store.rows(),
            1000,
            "no row of the changed shape lands"
        );
        assert_eq!(m.edits().cell(5, 2), Some(&EditValue::Text("w".into())));
        assert_eq!(m.notice(), Some(COLUMNS_BLOCKED));
        assert!(!m.can_save());
        assert_eq!(m.continuation(), None);
    });
    assert_eq!(names(cx, &m), ["id", "g", "v"]);
    // Running the table again, which drops the edits, describes it afresh.
    let sql = m.read_with(cx, |m, _| m.browse().unwrap().sql().to_owned());
    m.update(cx, |m, cx| m.run(sql, cx));
    loaded_as(cx, &m, &["id", "g", "v", "extra"]);
    m.read_with(cx, |m, _| {
        assert!(m.edits().is_empty());
        assert_eq!(m.notice(), Some(COLUMNS_RELOADED));
    });
    assert!(m.update(cx, |m, cx| m.stage_cell(5, 3, "x", cx)));
    assert!(
        m.read_with(cx, |m, _| m.can_save()),
        "the fresh load lifts the stop"
    );
}

#[gpui_kit::test]
fn a_sorted_postgres_continuation_after_a_reorder_loads_fresh(cx: &mut TestAppContext) {
    let rt = runtime(cx);
    let _enter = rt.enter();
    let (db, _dir, m) = pg_browse(cx, &rt, PG, tuning());
    ended(cx, &m);
    assert!(m.update(cx, |m, cx| m.cycle_sort("v", cx)));
    ended(cx, &m);
    rt.block_on(async {
        db.admin()
            .await
            .batch_execute("ALTER TABLE t DROP COLUMN g; ALTER TABLE t ADD COLUMN g int")
            .await
            .unwrap()
    });
    assert!(m.update(cx, |m, cx| m.continue_browse(cx)));
    loaded_as(cx, &m, &["id", "v", "g"]);
    m.read_with(cx, |m, _| {
        assert_eq!(m.results()[0].store.rows(), 1000);
        assert_eq!(m.notice(), Some(COLUMNS_RELOADED));
        assert!(m.browse().unwrap().sql().contains("ORDER BY v ASC"));
    });
}

#[gpui_kit::test]
fn a_conflict_reload_after_a_change_shows_no_fresh_value_and_stops_saving(cx: &mut TestAppContext) {
    let fx = fixture(SMALL, "");
    let m = ready(cx, &fx, "t");
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 1, "z", cx)));
    exec(&fx, "DELETE FROM t WHERE id = 1");
    assert!(m.update(cx, |m, cx| m.save(cx)));
    wait_idle(cx, &m);
    wait_unlocked(cx, &fx);
    exec(
        &fx,
        "INSERT INTO t VALUES (1, 'q'); ALTER TABLE t RENAME COLUMN name TO label",
    );
    assert!(m.update(cx, |m, cx| m.reload_conflicts(cx)));
    wait_real(cx, "the reload", |cx| {
        m.read_with(cx, |m, _| !m.reloading())
    });
    m.read_with(cx, |m, _| {
        assert_eq!(m.fresh_at(GridRow::Loaded(0), 1), None);
        assert_eq!(m.notice(), Some(COLUMNS_BLOCKED));
        assert_eq!(m.edits().len(), 1, "staged edits stay");
        assert!(!m.can_save());
    });
}

#[gpui_kit::test]
fn a_postgres_conflict_reload_after_a_reorder_stops_saving(cx: &mut TestAppContext) {
    let rt = runtime(cx);
    let _enter = rt.enter();
    let setup = "CREATE TABLE t(id int PRIMARY KEY, a text, b text); \
                 INSERT INTO t VALUES (1, 'a1', 'b1'), (2, 'a2', 'b2');";
    let (db, _dir, m) = pg_browse(cx, &rt, setup, RunnerTuning::default());
    wait_real(cx, "an editable browse", |cx| {
        m.read_with(cx, |m, _| m.editable() && !m.running())
    });
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 1, "z", cx)));
    let admin = |sql: &'static str| {
        rt.block_on(async { db.admin().await.batch_execute(sql).await.unwrap() })
    };
    admin("DELETE FROM t WHERE id = 1");
    assert!(m.update(cx, |m, cx| m.save(cx)));
    wait_idle(cx, &m);
    assert_eq!(
        m.read_with(cx, |m, _| m.conflicts().to_vec()),
        [GridRow::Loaded(0)]
    );
    admin(
        "INSERT INTO t VALUES (1, 'a9', 'b9'); \
         ALTER TABLE t DROP COLUMN a; ALTER TABLE t ADD COLUMN a text",
    );
    assert!(m.update(cx, |m, cx| m.reload_conflicts(cx)));
    wait_real(cx, "the reload", |cx| {
        m.read_with(cx, |m, _| !m.reloading())
    });
    m.read_with(cx, |m, _| {
        assert_eq!(
            m.fresh_at(GridRow::Loaded(0), 1),
            None,
            "b9 is not shown under a"
        );
        assert_eq!(m.notice(), Some(COLUMNS_BLOCKED));
        assert!(!m.can_save());
    });
}
