//! A save beside a parked stream stops the stream first; a run, a cleared browse or a
//! closed tab abandons a save in flight and rolls back; a run while a commit settles is
//! refused, so a failed commit keeps its edits.
use std::sync::Arc;
use std::time::Duration;

use crate::support::{
    Fx, browsing, fixture, init_tokio, ready, value_of, wait_idle, wait_prompt, wait_real,
    wait_unlocked,
};
use dbento_app::browse::BrowseEvent;
use dbento_app::runner::{RunnerSpec, RunnerTuning, StatementEnd};
use dbento_app::tab_model::{SaveState, TabModel};
use dbento_core::{CellRef, RelationRef, Workspace};
use gpui_kit::{AppContext as _, Entity, TestAppContext};

const T: &str = "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT); \
                 INSERT INTO t VALUES (1, 'b'), (2, 'a'), (3, 'c');";
const BIG: &str = "CREATE TABLE big(id INTEGER PRIMARY KEY, v TEXT); \
                   WITH RECURSIVE r(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM r WHERE x < 5000) \
                   INSERT INTO big SELECT x, 'v' FROM r;";

fn slow_idle() -> RunnerTuning {
    RunnerTuning {
        idle_complete: Duration::from_secs(120),
        ..RunnerTuning::default()
    }
}
fn parked(cx: &mut TestAppContext, fx: &Fx) -> Entity<TabModel> {
    let m = browsing(cx, fx, "big", slow_idle(), "");
    // Past the table's size, so the stream parks at the fetch window, not at the cap.
    m.update(cx, |m, _| m.set_max_rows(Some(100_000)));
    wait_real(cx, "the parked stream", |cx| {
        m.read_with(cx, |m, _| m.editable() && m.paused().is_some())
    });
    m
}
fn stopped(cx: &mut TestAppContext, m: &Entity<TabModel>) -> bool {
    m.read_with(cx, |m, _| {
        matches!(
            m.results().first().and_then(|r| r.store.end()),
            Some(StatementEnd::Stopped { .. })
        )
    })
}

#[gpui_kit::test]
fn a_parked_stream_is_stopped_before_the_save(cx: &mut TestAppContext) {
    let fx = fixture(BIG, "");
    let m = parked(cx, &fx);
    assert!(!m.read_with(cx, |m, _| m.can_save()), "nothing staged yet");
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 1, "w", cx)));
    assert!(m.read_with(cx, |m, _| m.can_save()));
    m.update(cx, |m, cx| {
        assert!(m.save(cx));
        assert_eq!(m.save_state(), SaveState::Stopping);
    });
    wait_prompt(cx, &m);
    assert!(stopped(cx, &m), "the stream was stopped, not left parked");
    m.update(cx, |m, cx| m.commit(cx));
    wait_idle(cx, &m);
    assert_eq!(
        m.read_with(cx, |m, _| m.edit_banner().map(str::to_owned)),
        None
    );
    assert_eq!(value_of(&fx, "big", "v", 1), "w");
}

#[gpui_kit::test]
fn a_stream_parked_at_its_cap_is_stopped_before_the_save(cx: &mut TestAppContext) {
    let fx = fixture(BIG, "");
    // The default 1,000-row cap parks the 5,000-row browse at its cap.
    let m = browsing(cx, &fx, "big", slow_idle(), "");
    wait_real(cx, "the stream parked at its cap", |cx| {
        m.read_with(cx, |m, _| m.editable() && m.parked_at_cap().is_some())
    });
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 1, "w", cx)));
    m.update(cx, |m, cx| {
        assert!(m.save(cx));
        assert_eq!(m.save_state(), SaveState::Stopping);
    });
    wait_prompt(cx, &m);
    assert!(
        m.read_with(cx, |m, _| m.parked_at_cap().is_none()
            && m.results()[0].store.end().is_some()),
        "the stream was ended at its cap, not left parked"
    );
    m.update(cx, |m, cx| m.commit(cx));
    wait_idle(cx, &m);
    assert_eq!(value_of(&fx, "big", "v", 1), "w");
}

#[gpui_kit::test]
fn a_run_during_the_stop_abandons_the_save(cx: &mut TestAppContext) {
    let fx = fixture(BIG, "");
    let m = parked(cx, &fx);
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 1, "w", cx)));
    m.update(cx, |m, cx| {
        assert!(m.save(cx));
        m.run("SELECT 1".into(), cx);
        assert_eq!(m.save_state(), SaveState::Idle);
    });
    wait_real(cx, "the new run", |cx| m.read_with(cx, |m, _| !m.running()));
    m.read_with(cx, |m, _| {
        assert_eq!(m.save_state(), SaveState::Idle);
        assert!(m.apply_prompt().is_none());
        assert!(m.edits().is_empty());
    });
    assert_eq!(value_of(&fx, "big", "v", 1), "v");
}

#[gpui_kit::test]
fn a_reconnect_during_the_stop_still_saves(cx: &mut TestAppContext) {
    let fx = fixture(BIG, "");
    let m = parked(cx, &fx);
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 1, "w", cx)));
    m.update(cx, |m, cx| {
        assert!(m.save(cx));
        let ws = m.spec().workspace.clone();
        let _old = m.reconnect(ws, cx);
    });
    wait_prompt(cx, &m);
    m.update(cx, |m, cx| m.commit(cx));
    wait_idle(cx, &m);
    assert_eq!(value_of(&fx, "big", "v", 1), "w");
}

#[gpui_kit::test]
fn a_run_during_the_prompt_rolls_back(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = ready(cx, &fx, "t");
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 1, "z", cx)));
    assert!(m.update(cx, |m, cx| m.save(cx)));
    wait_prompt(cx, &m);
    m.update(cx, |m, cx| m.run("SELECT 1".into(), cx));
    m.read_with(cx, |m, _| {
        assert!(m.apply_prompt().is_none());
        assert_eq!(m.save_state(), SaveState::Idle);
    });
    wait_unlocked(cx, &fx);
    assert_eq!(value_of(&fx, "t", "name", 1), "b");
}

#[gpui_kit::test]
fn clearing_the_browse_during_the_prompt_rolls_back(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = ready(cx, &fx, "t");
    assert!(m.update(cx, |m, cx| m.delete_row(0, cx)));
    assert!(m.update(cx, |m, cx| m.save(cx)));
    wait_prompt(cx, &m);
    m.update(cx, |m, cx| m.clear_browse(cx));
    assert!(m.read_with(cx, |m, _| m.apply_prompt().is_none()));
    wait_unlocked(cx, &fx);
    assert_eq!(fx.count("SELECT count(*) FROM t"), 3);
}

#[gpui_kit::test]
fn closing_the_tab_during_the_prompt_rolls_back(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = ready(cx, &fx, "t");
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 1, "z", cx)));
    assert!(m.update(cx, |m, cx| m.save(cx)));
    wait_prompt(cx, &m);
    let _closing = m.update(cx, |m, cx| m.close(cx));
    assert!(m.read_with(cx, |m, _| m.apply_prompt().is_none()));
    wait_unlocked(cx, &fx);
    assert_eq!(value_of(&fx, "t", "name", 1), "b");
}

#[gpui_kit::test]
fn a_parked_postgres_stream_is_stopped_then_saved(cx: &mut TestAppContext) {
    init_tokio(cx);
    let rt = cx.update(|cx| dbento_app::tokio_bridge::handle(cx));
    let _enter = rt.enter();
    let db = rt.block_on(dbento_testkit::pg_db());
    rt.block_on(async {
        db.admin()
            .await
            .batch_execute(
                "DROP TABLE IF EXISTS m5c1_big; \
                 CREATE TABLE m5c1_big(id int PRIMARY KEY, v text); \
                 INSERT INTO m5c1_big SELECT x, 'v' FROM generate_series(1, 5000) x;",
            )
            .await
            .unwrap()
    });
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("dbento.toml"),
        format!("dbento = 1\n{}", db.connection_toml("pg", "")),
    )
    .unwrap();
    let pw = db.password().to_owned();
    let spec = RunnerSpec {
        workspace: Arc::new(Workspace::open(dir.path()).unwrap()),
        connection: "pg".into(),
        store: Arc::new(dbento_core::MemoryStore::new()),
        env: Arc::new(move |v| (v == dbento_testkit::SECRET_VAR).then(|| pw.clone())),
    };
    let m = cx.new(|_| TabModel::with_tuning(spec, slow_idle()));
    // A cap past the table's size parks at the fetch window instead of the cap.
    m.update(cx, |m, _| m.set_max_rows(Some(100_000)));
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
        m.load_target(RelationRef::new("public", "m5c1_big"), cx)
    });
    wait_real(cx, "the parked stream", |cx| {
        m.read_with(cx, |m, _| m.editable() && m.paused().is_some())
    });
    let id = m
        .read_with(cx, |m, _| match m.results()[0].store.cell(0, 0) {
            Some(CellRef::Text(s)) => Some(s.to_owned()),
            _ => None,
        })
        .unwrap();
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 1, "w", cx)));
    assert!(m.update(cx, |m, cx| m.save(cx)));
    wait_prompt(cx, &m);
    assert!(stopped(cx, &m));
    m.update(cx, |m, cx| m.commit(cx));
    wait_idle(cx, &m);
    assert_eq!(
        m.read_with(cx, |m, _| m.edit_banner().map(str::to_owned)),
        None
    );
    let v = rt.block_on(db.scalar(&format!("SELECT v FROM m5c1_big WHERE id = {id}")));
    assert_eq!(v.as_deref(), Some("w"));
}

/// The bridge's runtime, which the caller keeps entered so the container can drop.
fn pg_runtime(cx: &mut TestAppContext) -> tokio::runtime::Handle {
    init_tokio(cx);
    cx.update(|cx| dbento_app::tokio_bridge::handle(cx))
}
/// A Postgres browse of `keep_child` with one change staged and applied, at the prompt; the
/// change breaks a deferred key, so it passes the UPDATE and fails only at COMMIT.
fn at_failing_commit(
    cx: &mut TestAppContext,
    rt: &tokio::runtime::Handle,
) -> (Entity<TabModel>, dbento_testkit::PgDb) {
    let db = rt.block_on(dbento_testkit::pg_db());
    rt.block_on(async {
        db.admin()
            .await
            .batch_execute(
                "DROP TABLE IF EXISTS keep_child, keep_parent; \
                 CREATE TABLE keep_parent(id int PRIMARY KEY); \
                 CREATE TABLE keep_child(id int PRIMARY KEY, \
                   p int REFERENCES keep_parent DEFERRABLE INITIALLY DEFERRED); \
                 INSERT INTO keep_parent VALUES (1); INSERT INTO keep_child VALUES (1, 1);",
            )
            .await
            .unwrap()
    });
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("dbento.toml"),
        format!("dbento = 1\n{}", db.connection_toml("pg", "")),
    )
    .unwrap();
    let pw = db.password().to_owned();
    let spec = RunnerSpec {
        workspace: Arc::new(Workspace::open(dir.path()).unwrap()),
        connection: "pg".into(),
        store: Arc::new(dbento_core::MemoryStore::new()),
        env: Arc::new(move |v| (v == dbento_testkit::SECRET_VAR).then(|| pw.clone())),
    };
    let m = cx.new(|_| TabModel::with_tuning(spec, RunnerTuning::default()));
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
        m.load_target(RelationRef::new("public", "keep_child"), cx)
    });
    wait_real(cx, "an editable browse", |cx| {
        m.read_with(cx, |m, _| m.editable() && !m.running())
    });
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 1, "999", cx)));
    assert!(m.update(cx, |m, cx| m.save(cx)));
    wait_prompt(cx, &m);
    (m, db)
}
fn child_p(db: &dbento_testkit::PgDb, rt: &tokio::runtime::Handle) -> Option<String> {
    rt.block_on(db.scalar("SELECT p::text FROM keep_child WHERE id = 1"))
}

#[gpui_kit::test]
fn a_run_while_a_failed_commit_settles_keeps_the_edits(cx: &mut TestAppContext) {
    let rt = pg_runtime(cx);
    let _enter = rt.enter();
    let (m, db) = at_failing_commit(cx, &rt);
    let run = m.read_with(cx, |m, _| m.run_id());
    // The run arrives before the COMMIT's outcome lands, while the save is finishing.
    m.update(cx, |m, cx| {
        m.commit(cx);
        assert_eq!(m.save_state(), SaveState::Finishing);
        m.run("SELECT 1".into(), cx);
    });
    wait_idle(cx, &m);
    m.read_with(cx, |m, _| {
        let banner = m.edit_banner().unwrap_or_default();
        assert!(banner.starts_with("Commit failed"), "{banner}");
        assert_eq!(m.edits().len(), 1, "the banner's edits are still staged");
        assert_eq!(m.run_id(), run, "the run was refused");
    });
    assert_eq!(child_p(&db, &rt).as_deref(), Some("1"));
}

#[gpui_kit::test]
fn discard_runs_while_a_commit_settles(cx: &mut TestAppContext) {
    let rt = pg_runtime(cx);
    let _enter = rt.enter();
    let (m, _db) = at_failing_commit(cx, &rt);
    let run = m.read_with(cx, |m, _| m.run_id());
    let id = m.update(cx, |m, cx| {
        m.commit(cx);
        assert!(
            !m.request_run("SELECT 1".into(), cx),
            "staged edits ask first"
        );
        m.discard_question().unwrap().id
    });
    m.update(cx, |m, cx| m.answer_discard(id, true, cx));
    m.read_with(cx, |m, _| {
        assert!(m.edits().is_empty());
        assert_eq!(m.run_id(), run + 1, "Discard runs at once");
    });
    wait_idle(cx, &m);
}
