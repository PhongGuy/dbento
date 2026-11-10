//! Saving a query tab whose file changed on disk since it was opened asks Reload, Overwrite
//! or Cancel instead of writing over the change.
use crate::support::{Opened, click_id, opened, palette_pick, palette_rows, present, wait_real};
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
    assert_eq!(
        sql(cx, o),
        "SELECT 1",
        "the saved query opens with its text"
    );
    file
}
/// Types `text` as the active tab's SQL and presses Ctrl+S in its editor.
fn save(cx: &mut TestAppContext, o: &Opened, text: &str) {
    let tab = o.shell.read_with(cx, |s, _| s.tab().unwrap().clone());
    let e = tab.read_with(cx, |t, _| t.editor().clone());
    cx.update_window(o.win, |_, w, cx| {
        tab.update(cx, |t, cx| t.set_sql(text, w, cx));
        e.update(cx, |e, cx| e.focus(w, cx));
        w.press("ctrl-s", cx);
    })
    .unwrap();
    cx.run_until_parked();
}
fn sql(cx: &mut TestAppContext, o: &Opened) -> String {
    o.shell
        .read_with(cx, |s, cx| s.tab().unwrap().read(cx).sql(cx))
}
fn edited(cx: &mut TestAppContext, o: &Opened) -> bool {
    o.shell
        .read_with(cx, |s, cx| s.tab().unwrap().read(cx).edited(cx))
}
fn asked(cx: &mut TestAppContext, o: &Opened) {
    wait_real(cx, "the conflict question", |cx| {
        present(cx, o.win, "save-conflict-cancel")
    });
    assert!(
        present(cx, o.win, "save-conflict-reload"),
        "the conflict offers reload"
    );
    assert!(
        present(cx, o.win, "save-conflict-overwrite"),
        "the conflict offers overwrite"
    );
}
fn changed_on_disk(cx: &mut TestAppContext) -> (Opened, PathBuf) {
    let o = opened(cx, "", "", 0);
    let file = open_file(cx, &o);
    fs::write(&file, "SELECT 9").unwrap();
    save(cx, &o, "SELECT 2");
    asked(cx, &o);
    (o, file)
}

#[gpui_kit::test]
fn cancel_leaves_the_changed_file_and_the_edited_text(cx: &mut TestAppContext) {
    let (o, file) = changed_on_disk(cx);
    click_id(cx, o.win, "save-conflict-cancel");
    assert!(!present(cx, o.win, "save-conflict-cancel"));
    assert_eq!(fs::read_to_string(&file).unwrap(), "SELECT 9");
    assert_eq!(sql(cx, &o), "SELECT 2");
    assert!(edited(cx, &o));
}

#[gpui_kit::test]
fn escape_cancels(cx: &mut TestAppContext) {
    let (o, file) = changed_on_disk(cx);
    cx.update_window(o.win, |_, w, cx| w.press("escape", cx))
        .unwrap();
    cx.run_until_parked();
    assert!(!present(cx, o.win, "save-conflict-cancel"));
    assert_eq!(fs::read_to_string(&file).unwrap(), "SELECT 9");
    assert_eq!(sql(cx, &o), "SELECT 2");
}

#[gpui_kit::test]
fn overwrite_writes_and_a_second_save_does_not_ask(cx: &mut TestAppContext) {
    let (o, file) = changed_on_disk(cx);
    click_id(cx, o.win, "save-conflict-overwrite");
    assert_eq!(fs::read_to_string(&file).unwrap(), "SELECT 2");
    assert!(!edited(cx, &o));
    save(cx, &o, "SELECT 3");
    assert!(!present(cx, o.win, "save-conflict-cancel"));
    assert_eq!(fs::read_to_string(&file).unwrap(), "SELECT 3");
}

#[gpui_kit::test]
fn reload_takes_the_disk_text(cx: &mut TestAppContext) {
    let (o, file) = changed_on_disk(cx);
    click_id(cx, o.win, "save-conflict-reload");
    assert_eq!(sql(cx, &o), "SELECT 9");
    assert!(!edited(cx, &o));
    assert_eq!(fs::read_to_string(&file).unwrap(), "SELECT 9");
    save(cx, &o, "SELECT 4");
    assert!(
        !present(cx, o.win, "save-conflict-cancel"),
        "the reloaded text is what the tab remembers"
    );
    assert_eq!(fs::read_to_string(&file).unwrap(), "SELECT 4");
}

#[gpui_kit::test]
fn an_unchanged_file_saves_without_asking(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let file = open_file(cx, &o);
    save(cx, &o, "SELECT 2");
    assert!(!present(cx, o.win, "confirm-dialog"));
    assert_eq!(fs::read_to_string(&file).unwrap(), "SELECT 2");
    save(cx, &o, "SELECT 3");
    assert!(!present(cx, o.win, "confirm-dialog"));
    assert_eq!(fs::read_to_string(&file).unwrap(), "SELECT 3");
}

#[gpui_kit::test]
fn a_deleted_file_is_recreated(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let file = open_file(cx, &o);
    fs::remove_file(&file).unwrap();
    save(cx, &o, "SELECT 2");
    assert!(!present(cx, o.win, "confirm-dialog"));
    assert_eq!(fs::read_to_string(&file).unwrap(), "SELECT 2");
}

#[gpui_kit::test]
fn an_unreadable_file_says_so_and_writes_nothing(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let file = open_file(cx, &o);
    fs::remove_file(&file).unwrap();
    fs::create_dir(&file).unwrap();
    save(cx, &o, "SELECT 2");
    wait_real(cx, "the error", |cx| present(cx, o.win, "save-error-ok"));
    assert!(!present(cx, o.win, "save-conflict-cancel"));
    assert!(file.is_dir());
    assert!(edited(cx, &o));
}
