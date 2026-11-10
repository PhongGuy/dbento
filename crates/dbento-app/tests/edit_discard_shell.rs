//! Closing a tab, switching workspaces or closing the window with staged edits asks first.
use crate::support;
use crate::support::{Opened, label_of, launched, present, wait_real};
use dbento_app::app::AppPaths;
use dbento_core::RelationKind;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext, VisualTestContext};

const T: &str = "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT); \
                 INSERT INTO t VALUES (1, 'b'), (2, 'a');";
const T_SQL: &str = "SELECT *\nFROM main.t\nORDER BY id ASC";

fn t(cx: &mut TestAppContext) -> Opened {
    let o = launched(cx, T, "");
    o.open_relation(cx, "t", RelationKind::Table);
    o.wait_browse(cx, T_SQL);
    o
}
fn stage_z(cx: &mut TestAppContext, o: &Opened) {
    let m = o.model(cx);
    assert!(
        m.update(cx, |m, cx| m.stage_cell(0, 1, "z", cx)),
        "the cell stages"
    );
    cx.run_until_parked();
}
fn click(cx: &mut TestAppContext, o: &Opened, id: &'static str) {
    cx.update_window(o.win, |_, w, cx| w.click(id, cx)).unwrap();
    cx.run_until_parked();
}
fn dialog(cx: &mut TestAppContext, o: &Opened) {
    wait_real(cx, "the question", |cx| {
        present(cx, o.win, "confirm-dialog")
    });
}

#[gpui_kit::test]
fn closing_a_tab_with_staged_edits_asks(cx: &mut TestAppContext) {
    let o = t(cx);
    stage_z(cx, &o);
    let shell = o.shell.clone();
    let close = |cx: &mut TestAppContext| {
        cx.update_window(o.win, |_, w, cx| {
            shell.update(cx, |s, cx| s.close_tab_at(0, w, cx))
        })
        .unwrap();
        cx.run_until_parked();
    };
    close(cx);
    dialog(cx, &o);
    assert_eq!(
        label_of(cx, o.win, "confirm-detail").as_deref(),
        Some("Closing the tab drops them.")
    );
    click(cx, &o, "discard-keep");
    assert_eq!(o.shell.read_with(cx, |s, _| s.tabs().len()), 1);
    close(cx);
    dialog(cx, &o);
    click(cx, &o, "discard-close");
    assert_eq!(o.shell.read_with(cx, |s, _| s.tabs().len()), 0);
}

#[gpui_kit::test]
fn a_middle_click_on_a_tab_with_staged_edits_asks_and_keep_keeps_it(cx: &mut TestAppContext) {
    let o = t(cx);
    stage_z(cx, &o);
    support::middle_click(cx, o.win, ("tab", 0usize));
    dialog(cx, &o);
    assert_eq!(
        label_of(cx, o.win, "confirm-detail").as_deref(),
        Some("Closing the tab drops them.")
    );
    click(cx, &o, "discard-keep");
    assert_eq!(o.shell.read_with(cx, |s, _| s.tabs().len()), 1);
}

#[gpui_kit::test]
fn closing_a_tab_with_edited_sql_and_staged_edits_names_both(cx: &mut TestAppContext) {
    let o = t(cx);
    stage_z(cx, &o);
    let tab = o.shell.read_with(cx, |s, _| s.tab().unwrap().clone());
    cx.update_window(o.win, |_, w, cx| {
        tab.update(cx, |t, cx| t.set_sql("SELECT 1", w, cx))
    })
    .unwrap();
    cx.run_until_parked();
    assert!(
        tab.read_with(cx, |t, cx| t.edited(cx)),
        "set_sql leaves the browse but edits the SQL"
    );
    let shell = o.shell.clone();
    cx.update_window(o.win, |_, w, cx| {
        shell.update(cx, |s, cx| s.close_tab_at(0, w, cx))
    })
    .unwrap();
    cx.run_until_parked();
    dialog(cx, &o);
    assert_eq!(
        label_of(cx, o.win, "confirm-detail").as_deref(),
        Some("Closing the tab drops them. Its SQL is not saved anywhere.")
    );
    click(cx, &o, "discard-close");
    assert_eq!(o.shell.read_with(cx, |s, _| s.tabs().len()), 0);
}

#[gpui_kit::test]
fn switching_workspaces_with_staged_edits_asks(cx: &mut TestAppContext) {
    let o = t(cx);
    stage_z(cx, &o);
    let shell = o.shell.clone();
    let other = o.files.path().to_path_buf();
    cx.update_window(o.win, |_, w, cx| {
        shell.update(cx, |s, cx| s.switch_workspace(other, w, cx))
    })
    .unwrap();
    cx.run_until_parked();
    dialog(cx, &o);
    assert_eq!(
        label_of(cx, o.win, "confirm-detail").as_deref(),
        Some("Switching workspaces drops them.")
    );
    click(cx, &o, "discard-keep");
    assert_eq!(o.shell.read_with(cx, |s, _| s.tabs().len()), 1);
    assert_eq!(o.model(cx).read_with(cx, |m, _| m.edits().len()), 1);
}

#[gpui_kit::test]
fn quitting_with_staged_edits_asks(cx: &mut TestAppContext) {
    let o = t(cx);
    stage_z(cx, &o);
    let mut v = VisualTestContext::from_window(o.win, cx);
    assert!(!v.simulate_close(), "the question holds the window");
    dialog(cx, &o);
    assert_eq!(
        label_of(cx, o.win, "confirm-detail").as_deref(),
        Some("Closing Dbento drops them.")
    );
    click(cx, &o, "discard-keep");
    assert!(!cx.update(|cx| cx.windows().is_empty()));
    assert_eq!(o.model(cx).read_with(cx, |m, _| m.edits().len()), 1);
}

// Stages an edit and opens its apply prompt, which holds the edits in a transaction.
fn prompting(cx: &mut TestAppContext) -> Opened {
    let o = t(cx);
    stage_z(cx, &o);
    click(cx, &o, "save-edits");
    wait_real(cx, "the apply prompt", |cx| {
        present(cx, o.win, "apply-commit")
    });
    o
}

#[gpui_kit::test]
fn quitting_during_an_apply_prompt_asks(cx: &mut TestAppContext) {
    let o = prompting(cx);
    let mut v = VisualTestContext::from_window(o.win, cx);
    assert!(!v.simulate_close(), "the question holds the window");
    wait_real(cx, "the question", |cx| present(cx, o.win, "discard-close"));
    click(cx, &o, "discard-keep");
    assert!(!cx.update(|cx| cx.windows().is_empty()));
    assert_eq!(o.model(cx).read_with(cx, |m, _| m.edits().len()), 1);
}

#[gpui_kit::test]
fn switching_workspaces_during_an_apply_prompt_asks(cx: &mut TestAppContext) {
    let o = prompting(cx);
    let shell = o.shell.clone();
    let other = o.files.path().to_path_buf();
    cx.update_window(o.win, |_, w, cx| {
        shell.update(cx, |s, cx| s.switch_workspace(other, w, cx))
    })
    .unwrap();
    cx.run_until_parked();
    wait_real(cx, "the question", |cx| present(cx, o.win, "discard-close"));
    click(cx, &o, "discard-keep");
    assert_eq!(o.shell.read_with(cx, |s, _| s.tabs().len()), 1);
    assert_eq!(o.model(cx).read_with(cx, |m, _| m.edits().len()), 1);
}

#[gpui_kit::test]
fn quitting_with_staged_edits_and_a_lost_draft_names_both(cx: &mut TestAppContext) {
    let o = t(cx);
    stage_z(cx, &o);
    let drafts = AppPaths::in_dir(o.files.path()).drafts;
    let _ = std::fs::remove_dir_all(&drafts);
    std::fs::write(&drafts, "").unwrap(); // a file where the folder goes
    let tab = o.shell.read_with(cx, |s, _| s.tab().unwrap().clone());
    cx.update_window(o.win, |_, w, cx| {
        tab.update(cx, |t, cx| t.set_sql("SELECT 1", w, cx))
    })
    .unwrap();
    cx.run_until_parked();
    let mut v = VisualTestContext::from_window(o.win, cx);
    assert!(!v.simulate_close());
    dialog(cx, &o);
    assert_eq!(
        label_of(cx, o.win, "confirm-detail").as_deref(),
        Some("Closing Dbento drops them. 1 tab could not be kept as a draft.")
    );
}
