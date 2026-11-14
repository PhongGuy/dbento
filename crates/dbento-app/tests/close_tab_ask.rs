//! An edited tab shows a dot where its close button sits; closing a saved query's edited tab
//! offers Save, and closing a running tab says its statement is cancelled.
use crate::support::{
    LONG, Opened, click_id, label_of, opened, palette_pick, palette_rows, present, wait_real,
};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext};
use std::fs;
use std::path::PathBuf;

/// Opens `queries/lite/a.sql` holding `SELECT 1` in a new tab and returns its path.
fn open_file(cx: &mut TestAppContext, o: &Opened) -> PathBuf {
    let file = o.fx.dir.path().join("queries/lite/a.sql");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, "SELECT 1").unwrap();
    let e = o
        .shell
        .read_with(cx, |s, cx| s.tab().unwrap().read(cx).editor().clone());
    cx.update_window(o.win, |_, w, cx| {
        e.update(cx, |e, cx| e.focus(w, cx));
        w.press("ctrl-o", cx);
    })
    .unwrap();
    cx.run_until_parked();
    wait_real(cx, "the list", |cx| palette_rows(cx, o.win).len() == 1);
    palette_pick(cx, o.win, "queries/lite/a.sql");
    file
}
fn set_sql(cx: &mut TestAppContext, o: &Opened, text: &str) {
    let tab = o.shell.read_with(cx, |s, _| s.tab().unwrap().clone());
    cx.update_window(o.win, |_, w, cx| {
        tab.update(cx, |t, cx| t.set_sql(text, w, cx))
    })
    .unwrap();
    cx.run_until_parked();
}
fn active(cx: &mut TestAppContext, o: &Opened) -> usize {
    o.shell.read_with(cx, |s, _| s.active().unwrap())
}
fn tabs(cx: &mut TestAppContext, o: &Opened) -> usize {
    o.shell.read_with(cx, |s, _| s.tabs().len())
}
/// Asks to close tab `ix` and returns the question's detail once it is on screen.
fn close(cx: &mut TestAppContext, o: &Opened, ix: usize) -> Option<String> {
    let shell = o.shell.clone();
    cx.update_window(o.win, |_, w, cx| {
        shell.update(cx, |s, cx| s.close_tab_at(ix, w, cx))
    })
    .unwrap();
    cx.run_until_parked();
    wait_real(cx, "the question", |cx| {
        present(cx, o.win, "confirm-dialog")
    });
    label_of(cx, o.win, "confirm-detail")
}
// The run's SQL counts as saved, so only the running statement can raise the question.
fn run_long(cx: &mut TestAppContext, o: &Opened) {
    let m = o.model(cx);
    o.run(cx, LONG);
    let tab = o.shell.read_with(cx, |s, _| s.tab().unwrap().clone());
    cx.update(|cx| tab.update(cx, |t, cx| t.mark_saved(cx)));
    wait_real(cx, "the running row", |cx| {
        m.read_with(cx, |m, _| !m.results().is_empty() && m.running())
    });
}

#[gpui_kit::test]
fn an_edited_tab_shows_the_unsaved_dot_and_a_clean_one_does_not(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    assert!(
        !present(cx, o.win, ("tab-dirty", 0usize)),
        "a new tab is clean"
    );
    assert!(present(cx, o.win, ("tab-close", 0usize)));
    set_sql(cx, &o, "SELECT 2");
    assert_eq!(
        label_of(cx, o.win, ("tab-dirty", 0usize)).as_deref(),
        Some("Unsaved")
    );
    assert!(
        present(cx, o.win, ("tab-close", 0usize)),
        "the close button keeps its place under the dot"
    );
}

#[gpui_kit::test]
fn a_saved_query_tab_loses_its_dot_once_saved(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    open_file(cx, &o);
    let ix = active(cx, &o);
    assert!(!present(cx, o.win, ("tab-dirty", ix)));
    set_sql(cx, &o, "SELECT 2");
    assert!(present(cx, o.win, ("tab-dirty", ix)));
    let e = o
        .shell
        .read_with(cx, |s, cx| s.tab().unwrap().read(cx).editor().clone());
    cx.update_window(o.win, |_, w, cx| {
        e.update(cx, |e, cx| e.focus(w, cx));
        w.press("ctrl-s", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert!(!present(cx, o.win, ("tab-dirty", ix)));
}

#[gpui_kit::test]
fn closing_an_edited_saved_query_offers_save_discard_and_keep(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let file = open_file(cx, &o);
    set_sql(cx, &o, "SELECT 2");
    let ix = active(cx, &o);
    let n = tabs(cx, &o);
    assert_eq!(
        close(cx, &o, ix).as_deref(),
        Some("Changes to a.sql are not saved.")
    );
    assert!(present(cx, o.win, "discard-save"));
    assert!(present(cx, o.win, "discard-close"));
    click_id(cx, o.win, "discard-keep");
    assert_eq!(tabs(cx, &o), n, "Keep keeps the tab");
    assert_eq!(fs::read_to_string(&file).unwrap(), "SELECT 1");
    close(cx, &o, ix);
    click_id(cx, o.win, "discard-close");
    assert_eq!(tabs(cx, &o), n - 1, "Discard closes the tab");
    assert_eq!(
        fs::read_to_string(&file).unwrap(),
        "SELECT 1",
        "Discard writes nothing"
    );
}

#[gpui_kit::test]
fn save_on_the_close_question_writes_the_file_then_closes(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let file = open_file(cx, &o);
    set_sql(cx, &o, "SELECT 2");
    let ix = active(cx, &o);
    let n = tabs(cx, &o);
    close(cx, &o, ix);
    click_id(cx, o.win, "discard-save");
    assert_eq!(fs::read_to_string(&file).unwrap(), "SELECT 2");
    assert_eq!(tabs(cx, &o), n - 1);
}

#[cfg(unix)]
#[gpui_kit::test]
fn a_failed_save_on_the_close_question_keeps_the_tab_and_shows_the_error(cx: &mut TestAppContext) {
    use std::os::unix::fs::PermissionsExt as _;
    let o = opened(cx, "", "", 0);
    let file = open_file(cx, &o);
    set_sql(cx, &o, "SELECT 2");
    let ix = active(cx, &o);
    let n = tabs(cx, &o);
    close(cx, &o, ix);
    let dir = file.parent().unwrap().to_path_buf();
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).unwrap();
    click_id(cx, o.win, "discard-save");
    let shown = present(cx, o.win, "save-error-ok");
    fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(shown, "the write error is shown");
    assert_eq!(tabs(cx, &o), n, "the tab stays open");
    assert_eq!(fs::read_to_string(&file).unwrap(), "SELECT 1");
}

#[gpui_kit::test]
fn closing_an_edited_scratch_tab_keeps_its_question(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    set_sql(cx, &o, "SELECT 2");
    assert_eq!(
        close(cx, &o, 0).as_deref(),
        Some("Its SQL is not saved anywhere.")
    );
    assert!(!present(cx, o.win, "discard-save"), "nothing to save it to");
    click_id(cx, o.win, "discard-keep");
}

#[gpui_kit::test]
fn closing_a_running_tab_asks_and_close_closes_it(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    run_long(cx, &o);
    assert_eq!(
        close(cx, &o, 0).as_deref(),
        Some("Closing cancels the statement running in Query 1.")
    );
    click_id(cx, o.win, "discard-keep");
    assert_eq!(tabs(cx, &o), 1);
    assert!(
        o.model(cx).read_with(cx, |m, _| m.running()),
        "Keep cancels nothing"
    );
    close(cx, &o, 0);
    click_id(cx, o.win, "discard-close");
    assert_eq!(tabs(cx, &o), 0);
}

#[gpui_kit::test]
fn closing_a_running_edited_tab_names_both_in_one_question(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    run_long(cx, &o);
    set_sql(cx, &o, "SELECT 2");
    assert_eq!(
        close(cx, &o, 0).as_deref(),
        Some("Closing cancels the statement running in Query 1. Its SQL is not saved anywhere.")
    );
    click_id(cx, o.win, "discard-close");
    assert!(
        !present(cx, o.win, "confirm-dialog"),
        "no second question follows"
    );
    assert_eq!(tabs(cx, &o), 0);
}
