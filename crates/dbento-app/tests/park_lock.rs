//! A commit to a SQLite file is not blocked by another tab's browse parked on that file.
use crate::support;
use std::time::Duration;

use crate::support::{
    Fx, browsing, fixture, init_tokio, spec, value_of, wait_idle, wait_prompt, wait_real,
};
use dbento_app::browse::BrowseEvent;
use dbento_app::runner::{RunnerTuning, StatementEnd};
use dbento_app::tab_model::{CapView, Continuation, Expiry, TabModel};
use dbento_core::RelationRef;
use gpui_kit::{AppContext as _, Entity, TestAppContext};

const TABLES: &str = "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT); \
                      INSERT INTO t VALUES (1, 'b'), (2, 'a'), (3, 'c'); \
                      CREATE TABLE big(id INTEGER PRIMARY KEY, v TEXT); \
                      WITH RECURSIVE r(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM r WHERE x < 5000) \
                      INSERT INTO big SELECT x, 'v' FROM r;";

fn slow_idle() -> RunnerTuning {
    RunnerTuning {
        idle_complete: Duration::from_secs(120),
        ..RunnerTuning::default()
    }
}

/// A second tab browsing `table`, editable and idle. Unlike `support::ready` it keeps the
/// tokio runtime the first tab's runner lives on.
fn second(cx: &mut TestAppContext, fx: &Fx, table: &str) -> Entity<TabModel> {
    let m = cx.new(|_| TabModel::new(spec(fx, "lite")));
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
        m.load_target(RelationRef::new("main", table), cx)
    });
    wait_real(cx, "an editable browse", |cx| {
        m.read_with(cx, |m, _| m.editable() && !m.running())
    });
    m
}

/// Stages `name = 'z'` on row 1 of `writer`'s `t`, saves and commits; returns the banner.
fn commit_one(cx: &mut TestAppContext, writer: &Entity<TabModel>) -> Option<String> {
    assert!(
        writer.update(cx, |m, cx| m.stage_cell(0, 1, "z", cx)),
        "the writer stages its cell"
    );
    assert!(
        writer.update(cx, |m, cx| m.save(cx)),
        "the writer's save starts"
    );
    wait_prompt(cx, writer);
    writer.update(cx, |m, cx| m.commit(cx));
    wait_idle(cx, writer);
    writer.read_with(cx, |m, _| m.edit_banner().map(str::to_owned))
}

#[gpui_kit::test]
fn a_commit_releases_another_tab_parked_at_its_cap(cx: &mut TestAppContext) {
    let fx = fixture(TABLES, "");
    let reader = browsing(cx, &fx, "big", slow_idle(), "");
    wait_real(cx, "the other tab parked at its cap", |cx| {
        reader.read_with(cx, |m, _| m.parked_at_cap().is_some())
    });
    let writer = second(cx, &fx, "t");
    assert_eq!(commit_one(cx, &writer), None);
    assert_eq!(value_of(&fx, "t", "name", 1), "z");
    // The released browse offers Fetch next as a continuation from its last loaded row.
    wait_real(cx, "the released browse", |cx| {
        reader.read_with(cx, |m, _| m.continuation().is_some())
    });
    reader.read_with(cx, |m, _| {
        assert_eq!(
            m.cap_view(0),
            Some(CapView::Ended {
                expired: Some(Expiry::Released)
            })
        );
        assert_eq!(m.continuation(), Some(Continuation::Keyset));
        assert_eq!(m.results()[0].store.rows(), 1000);
    });
}

#[gpui_kit::test]
fn a_commit_lets_another_tab_paused_on_its_first_rows_load_to_its_end(cx: &mut TestAppContext) {
    let fx = fixture(TABLES, "");
    let reader = browsing(cx, &fx, "big", slow_idle(), "");
    // Past the table's size, so the stream parks at the fetch window, not at the cap.
    reader.update(cx, |m, _| m.set_max_rows(Some(100_000)));
    wait_real(cx, "the other tab paused", |cx| {
        reader.read_with(cx, |m, _| m.paused().is_some())
    });
    let writer = second(cx, &fx, "t");
    assert_eq!(commit_one(cx, &writer), None);
    assert_eq!(value_of(&fx, "t", "name", 1), "z");
    wait_real(cx, "the other tab's rows", |cx| {
        reader.read_with(cx, |m, _| {
            matches!(m.results()[0].store.end(), Some(StatementEnd::Complete(_)))
        })
    });
    assert_eq!(
        reader.read_with(cx, |m, _| m.results()[0].store.rows()),
        5000
    );
}

#[gpui_kit::test]
fn a_commit_leaves_a_park_on_another_file_alone(cx: &mut TestAppContext) {
    let other = fixture(TABLES, "");
    let reader = browsing(cx, &other, "big", slow_idle(), "");
    wait_real(cx, "the other file's tab parked at its cap", |cx| {
        reader.read_with(cx, |m, _| m.parked_at_cap().is_some())
    });
    let fx = fixture(TABLES, "");
    let writer = second(cx, &fx, "t");
    assert_eq!(commit_one(cx, &writer), None);
    assert!(reader.read_with(cx, |m, _| m.parked_at_cap().is_some()));
}

#[gpui_kit::test]
fn a_lock_held_outside_the_app_fails_the_commit_and_says_who_may_hold_it(cx: &mut TestAppContext) {
    let fx = fixture(TABLES, "");
    init_tokio(cx);
    let writer = second(cx, &fx, "t");
    // A reader mid-result in another program holds the file's shared lock.
    let outside = rusqlite::Connection::open(support::db(&fx)).unwrap();
    let mut statement = outside.prepare("SELECT * FROM big").unwrap();
    let mut rows = statement.query([]).unwrap();
    assert!(rows.next().unwrap().is_some());
    assert_eq!(
        commit_one(cx, &writer).as_deref(),
        Some(
            "Commit failed: another connection still holds the database file (a query or \
             transaction open in another tab, or another program). Nothing was committed."
        )
    );
    drop(rows);
    assert_eq!(value_of(&fx, "t", "name", 1), "b");
}
