//! The pending bar, the apply prompt and the rejection banner of a browse tab.
mod support;
use dbento_app::results_pane::save_label;
use dbento_app::tab_model::SaveState;
use dbento_core::{Change, EditBatch, EditValue, RelationKind, RowKey, render_sql};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext};
use support::{Opened, edit_target, label_of, launched, present, wait_real, wait_unlocked};

const T: &str = "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT); \
                 INSERT INTO t VALUES (1, 'b'), (2, 'a');";

fn t(cx: &mut TestAppContext) -> Opened {
    let o = launched(cx, T, "");
    o.open_relation(cx, "t", RelationKind::Table);
    o.wait_browse(cx, "SELECT *\nFROM main.t\nORDER BY id ASC");
    o
}
fn stage_z(cx: &mut TestAppContext, o: &Opened) {
    let m = o.model(cx);
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 1, "z", cx)));
    cx.run_until_parked();
}
fn click(cx: &mut TestAppContext, o: &Opened, id: &'static str) {
    cx.update_window(o.win, |_, w, cx| w.click(id, cx)).unwrap();
    cx.run_until_parked();
}

#[test]
fn the_save_button_says_saving_while_a_save_is_in_flight() {
    assert_eq!(save_label(SaveState::Idle), "Save");
    assert_eq!(save_label(SaveState::Prompt), "Save");
    for s in [
        SaveState::Stopping,
        SaveState::Applying,
        SaveState::Finishing,
    ] {
        assert_eq!(save_label(s), "Saving…");
    }
}

#[gpui_kit::test]
fn closing_the_tab_dismisses_the_prompt_and_rolls_back(cx: &mut TestAppContext) {
    let o = t(cx);
    stage_z(cx, &o);
    click(cx, &o, "save-edits");
    wait_real(cx, "the prompt", |cx| present(cx, o.win, "confirm-dialog"));
    let shell = o.shell.clone();
    cx.update_window(o.win, |_, w, cx| {
        shell.update(cx, |s, cx| s.close_tab_at(0, w, cx))
    })
    .unwrap();
    cx.run_until_parked();
    assert!(!present(cx, o.win, "confirm-dialog"));
    wait_unlocked(cx, &o.fx);
    assert_eq!(o.fx.count("SELECT count(*) FROM t WHERE name = 'z'"), 0);
}

#[gpui_kit::test]
fn the_bar_shows_while_something_is_staged_and_revert_clears_it(cx: &mut TestAppContext) {
    let o = t(cx);
    for id in ["pending-changes", "footer-pending"] {
        assert!(!present(cx, o.win, id), "{id}");
    }
    o.assert_inert(cx, "save-edits");
    o.assert_inert(cx, "revert-edits");
    stage_z(cx, &o);
    assert_eq!(
        label_of(cx, o.win, "pending-changes").as_deref(),
        Some("1 pending change")
    );
    assert_eq!(
        label_of(cx, o.win, "footer-pending").as_deref(),
        Some("1 pending change")
    );
    assert!(o.strip_enabled(cx, "save-edits"));
    click(cx, &o, "revert-edits");
    assert!(!present(cx, o.win, "pending-changes"));
    assert!(o.model(cx).read_with(cx, |m, _| m.edits().is_empty()));
}

#[gpui_kit::test]
fn save_asks_and_commit_applies_and_reloads(cx: &mut TestAppContext) {
    let o = t(cx);
    stage_z(cx, &o);
    click(cx, &o, "save-edits");
    wait_real(cx, "the prompt", |cx| present(cx, o.win, "confirm-dialog"));
    assert_eq!(
        label_of(cx, o.win, "confirm-detail").as_deref(),
        Some("Affects 1 row. Locks are held until you choose.")
    );
    let batch = EditBatch::new(
        edit_target(&o.fx, "t"),
        vec![Change::Update {
            key: RowKey::new(vec![("id".into(), EditValue::Text("1".into()))]),
            set: vec![("name".into(), EditValue::Text("z".into()))],
        }],
    )
    .unwrap();
    assert_eq!(label_of(cx, o.win, "confirm-sql"), Some(render_sql(&batch)));
    assert!(present(cx, o.win, "apply-rollback"));
    click(cx, &o, "apply-commit");
    wait_real(cx, "the commit and re-run", |cx| {
        !present(cx, o.win, "pending-changes")
            && label_of(cx, o.win, "cell-0-1").as_deref() == Some("z")
    });
    assert_eq!(o.fx.count("SELECT count(*) FROM t WHERE name = 'z'"), 1);
}

#[gpui_kit::test]
fn escape_rolls_back_and_keeps_the_edits(cx: &mut TestAppContext) {
    let o = t(cx);
    stage_z(cx, &o);
    click(cx, &o, "save-edits");
    wait_real(cx, "the prompt", |cx| present(cx, o.win, "confirm-dialog"));
    cx.update_window(o.win, |_, w, cx| w.press("escape", cx))
        .unwrap();
    let m = o.model(cx);
    wait_real(cx, "the roll back", |cx| {
        !present(cx, o.win, "confirm-dialog")
            && m.read_with(cx, |m, _| {
                m.save_state() == dbento_app::tab_model::SaveState::Idle
            })
    });
    assert_eq!(o.fx.count("SELECT count(*) FROM t WHERE name = 'z'"), 0);
    assert_eq!(
        label_of(cx, o.win, "pending-changes").as_deref(),
        Some("1 pending change")
    );
}

#[gpui_kit::test]
fn a_rejected_save_shows_the_banner_and_no_prompt(cx: &mut TestAppContext) {
    let o = t(cx);
    stage_z(cx, &o);
    rusqlite::Connection::open(o.fx.dir.path().join("data/app.db"))
        .unwrap()
        .execute_batch("DELETE FROM t WHERE id = 1")
        .unwrap();
    click(cx, &o, "save-edits");
    wait_real(cx, "the banner", |cx| present(cx, o.win, "edit-banner"));
    assert_eq!(
        label_of(cx, o.win, "edit-banner").as_deref(),
        Some("Nothing was committed: 1 row was modified by someone else")
    );
    assert!(!present(cx, o.win, "confirm-dialog"));
    assert!(present(cx, o.win, "pending-changes"));
}

#[gpui_kit::test]
fn the_save_button_stages_the_open_editor_first(cx: &mut TestAppContext) {
    let o = t(cx);
    stage_z(cx, &o);
    let pane = o
        .shell
        .read_with(cx, |s, cx| s.tab().unwrap().read(cx).results_pane().clone());
    cx.update_window(o.win, |_, w, cx| {
        pane.update(cx, |p, cx| assert!(p.begin_edit(1, 2, w, cx)))
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(o.win, |_, w, cx| w.input("typed", cx))
        .unwrap();
    cx.run_until_parked();
    click(cx, &o, "save-edits");
    let m = o.model(cx);
    assert_eq!(
        m.read_with(cx, |m, _| m.edits().cell(1, 1).cloned()),
        Some(EditValue::Text("typed".into())),
        "the open editor's text is staged before the save starts"
    );
    assert_eq!(pane.read_with(cx, |p, _| p.editing()), None);
    wait_real(cx, "the prompt", |cx| present(cx, o.win, "confirm-dialog"));
    click(cx, &o, "apply-rollback");
}
