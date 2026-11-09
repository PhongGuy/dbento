//! Which browse cells can be edited, and staging edits and deletes on the tab model.
use crate::support::{browsing, fixture, init_tokio, pg_ready, ready, wait_real};
use dbento_app::browse::EditBlock;
use dbento_app::runner::{RunnerTuning, SETTINGS_LOST};
use gpui_kit::TestAppContext;

const T: &str = "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT); \
                 INSERT INTO t VALUES (1, 'b'), (2, 'a'), (3, 'c');";

#[gpui_kit::test]
fn a_failed_browse_run_cannot_be_edited(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = browsing(cx, &fx, "t", RunnerTuning::default(), "DROP TABLE t");
    wait_real(cx, "the failed browse", |cx| {
        m.read_with(cx, |m, _| {
            m.browse().is_some()
                && !m.running()
                && m.results().first().is_some_and(|r| r.store.end().is_some())
        })
    });
    m.read_with(cx, |m, _| {
        assert!(!m.editable());
        assert_eq!(m.edit_block(), Some(EditBlock::RunFailed));
        assert_eq!(EditBlock::RunFailed.reason(), "the table query failed");
    });
    assert!(!m.update(cx, |m, cx| m.stage_cell(0, 1, "z", cx)));
}

#[gpui_kit::test]
fn a_read_only_connection_stages_nothing(cx: &mut TestAppContext) {
    let fx = fixture(T, "read_only = true");
    let m = browsing(cx, &fx, "t", RunnerTuning::default(), "");
    wait_real(cx, "the browse", |cx| {
        m.read_with(cx, |m, _| m.edit_block().is_some() && !m.running())
    });
    assert!(!m.read_with(cx, |m, _| m.editable()));
    assert!(!m.update(cx, |m, cx| m.stage_cell(0, 1, "z", cx)));
    assert!(!m.update(cx, |m, cx| m.delete_row(0, cx)));
}

#[gpui_kit::test]
fn the_editor_text_follows_the_cell_and_its_edit(cx: &mut TestAppContext) {
    let fx = fixture(
        "CREATE TABLE k(id INTEGER PRIMARY KEY, a TEXT, b BLOB); \
         INSERT INTO k VALUES (1, NULL, x'00'), (2, 'x' || char(10) || 'y', NULL);",
        "",
    );
    let m = ready(cx, &fx, "k");
    m.read_with(cx, |m, _| {
        assert_eq!(m.cell_edit_text(0, 0).as_deref(), Some("1"));
        assert_eq!(
            m.cell_edit_text(0, 1).as_deref(),
            Some(""),
            "NULL edits as empty text"
        );
        assert_eq!(
            m.cell_edit_text(0, 2),
            None,
            "bytes wait for the record view"
        );
        assert_eq!(
            m.cell_edit_text(1, 1),
            None,
            "multi-line text waits for the record view"
        );
        assert_eq!(m.cell_edit_text(9, 1), None);
    });
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 1, "", cx)));
    assert!(
        m.read_with(cx, |m, _| m.edits().is_empty()),
        "empty text over NULL is no change"
    );
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 1, "v", cx)));
    assert_eq!(
        m.read_with(cx, |m, _| m.cell_edit_text(0, 1)).as_deref(),
        Some("v")
    );
    assert!(m.update(cx, |m, cx| m.delete_row(0, cx)));
    assert_eq!(m.read_with(cx, |m, _| m.cell_edit_text(0, 1)), None);
}

#[gpui_kit::test]
fn a_locator_column_is_never_edited(cx: &mut TestAppContext) {
    let fx = fixture(
        "CREATE TABLE plain(a, b); INSERT INTO plain VALUES (1, 2);",
        "",
    );
    let m = ready(cx, &fx, "plain");
    m.read_with(cx, |m, _| {
        assert_eq!(m.cell_edit_text(0, 0), None, "rowid");
        assert_eq!(m.cell_edit_text(0, 1).as_deref(), Some("1"));
    });
    assert!(!m.update(cx, |m, cx| m.stage_cell(0, 0, "9", cx)));
}

#[gpui_kit::test]
fn revert_drops_every_edit(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = ready(cx, &fx, "t");
    m.update(cx, |m, cx| {
        assert!(m.stage_cell(0, 1, "z", cx));
        assert!(m.delete_row(2, cx));
        m.revert_edits(cx);
    });
    assert!(m.read_with(cx, |m, _| m.edits().is_empty()));
}

#[gpui_kit::test]
fn a_new_run_drops_the_edits(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = ready(cx, &fx, "t");
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 1, "z", cx)));
    assert!(m.update(cx, |m, cx| m.delete_row(2, cx)));
    assert_eq!(m.read_with(cx, |m, _| m.edits().len()), 2);
    m.update(cx, |m, cx| m.run("SELECT 1".into(), cx));
    assert!(m.read_with(cx, |m, _| m.edits().is_empty()));
}

/// Runs `sql` on the model and waits for the run to end.
fn run_to_end(
    cx: &mut TestAppContext,
    m: &gpui_kit::Entity<dbento_app::tab_model::TabModel>,
    sql: &str,
) {
    let before = m.read_with(cx, |m, _| m.run_id());
    m.update(cx, |m, cx| m.run(sql.to_owned(), cx));
    wait_real(cx, "the run", |cx| {
        m.read_with(cx, |m, _| {
            m.run_id() > before
                && !m.running()
                && m.results().first().is_some_and(|r| r.store.end().is_some())
        })
    });
}

#[gpui_kit::test]
fn a_postgres_session_whose_settings_changed_cannot_edit_until_it_reconnects(
    cx: &mut TestAppContext,
) {
    init_tokio(cx);
    let rt = cx.update(|cx| dbento_app::tokio_bridge::handle(cx));
    let _enter = rt.enter();
    let (_db, _dir, m) = pg_ready(
        cx,
        &rt,
        "CREATE TABLE t(id int PRIMARY KEY, name text); INSERT INTO t VALUES (1, 'a');",
        "t",
    );
    let browse_sql = m.read_with(cx, |m, _| m.browse().unwrap().sql().to_owned());
    // A SET changes how the session prints values, so its browse no longer matches apply's.
    run_to_end(cx, &m, "SET TIME ZONE 'Pacific/Chatham'");
    run_to_end(cx, &m, &browse_sql);
    m.read_with(cx, |m, _| {
        assert!(!m.editable());
        assert_eq!(m.edit_block(), Some(EditBlock::SettingsChanged));
    });
    assert_eq!(
        EditBlock::SettingsChanged.reason(),
        "session settings changed; reconnect to edit"
    );
    assert!(!m.update(cx, |m, cx| m.stage_cell(0, 1, "z", cx)));
    // A fresh session starts from the connection's settings again, once the first run after
    // the disconnect has said those settings were lost.
    drop(m.update(cx, |m, cx| m.disconnect(cx)));
    m.update(cx, |m, cx| m.run(browse_sql.clone(), cx));
    m.read_with(cx, |m, _| assert_eq!(m.notice(), Some(SETTINGS_LOST)));
    run_to_end(cx, &m, &browse_sql);
    m.read_with(cx, |m, _| {
        assert_eq!(m.edit_block(), None);
        assert!(m.editable());
    });
}
