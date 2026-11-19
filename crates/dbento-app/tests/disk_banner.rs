//! Open saved queries follow their files through the reload pass: clean tabs take the new
//! text, edited ones get the banner, and the app's own saves and moves change nothing.
use crate::support::{
    Opened, click_id, files_changed, label_of, open_saved, opened, present, settle_reload, tab_sql,
    wait_real,
};
use dbento_app::app::{AppPaths, SavedState};
use dbento_app::persist::load_json;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext};
use std::fs;
use std::path::{Path, PathBuf};

fn type_sql(cx: &mut TestAppContext, o: &Opened, text: &str) {
    let tab = o.shell.read_with(cx, |s, _| s.tab().unwrap().clone());
    cx.update_window(o.win, |_, w, cx| {
        tab.update(cx, |t, cx| t.set_sql(text, w, cx))
    })
    .unwrap();
    cx.run_until_parked();
}
fn press_save(cx: &mut TestAppContext, o: &Opened) {
    let e = o
        .shell
        .read_with(cx, |s, cx| s.tab().unwrap().read(cx).editor().clone());
    cx.update_window(o.win, |_, w, cx| {
        e.update(cx, |e, cx| e.focus(w, cx));
        w.press("ctrl-s", cx);
    })
    .unwrap();
    cx.run_until_parked();
}
fn banner(cx: &mut TestAppContext, o: &Opened) -> Option<String> {
    label_of(cx, o.win, "disk-banner-text")
}

#[gpui_kit::test]
fn a_clean_tab_takes_the_new_text(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let file = open_saved(cx, &o, "lite", "SELECT 1");
    fs::write(&file, "SELECT 2").unwrap();
    files_changed(cx, &o);
    assert_eq!(tab_sql(cx, &o), "SELECT 2");
    let disk = o.shell.read_with(cx, |s, cx| {
        s.tab().unwrap().read(cx).disk().map(str::to_owned)
    });
    assert_eq!(disk.as_deref(), Some("SELECT 2"), "disk follows");
    assert!(
        !present(cx, o.win, "disk-banner"),
        "no banner for a clean tab"
    );
}

#[gpui_kit::test]
fn an_edited_tab_shows_changed_and_reload_takes_the_disk(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let file = open_saved(cx, &o, "lite", "SELECT 1");
    type_sql(cx, &o, "SELECT 1 -- mine");
    fs::write(&file, "SELECT 2").unwrap();
    files_changed(cx, &o);
    assert_eq!(banner(cx, &o).as_deref(), Some("Changed on disk"));
    assert_eq!(
        tab_sql(cx, &o),
        "SELECT 1 -- mine",
        "edits are never overwritten unasked"
    );
    click_id(cx, o.win, "disk-reload");
    assert_eq!(tab_sql(cx, &o), "SELECT 2");
    assert!(!present(cx, o.win, "disk-banner"), "gone after Reload");
}

#[gpui_kit::test]
fn keep_mine_then_save_writes_without_asking(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let file = open_saved(cx, &o, "lite", "SELECT 1");
    type_sql(cx, &o, "SELECT 1 -- mine");
    fs::write(&file, "SELECT 2").unwrap();
    files_changed(cx, &o);
    click_id(cx, o.win, "disk-keep");
    files_changed(cx, &o);
    assert!(
        !present(cx, o.win, "disk-banner"),
        "stays hidden for the same disk text"
    );
    press_save(cx, &o);
    assert!(
        !present(cx, o.win, "save-conflict-cancel"),
        "no conflict question"
    );
    wait_real(cx, "the write", |_| {
        fs::read_to_string(&file).unwrap() == "SELECT 1 -- mine"
    });
    files_changed(cx, &o);
    assert!(
        !present(cx, o.win, "disk-banner"),
        "the own save reads back as no change"
    );
}

#[gpui_kit::test]
fn a_deleted_file_clears_when_it_reappears(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let file = open_saved(cx, &o, "lite", "SELECT 1");
    fs::remove_file(&file).unwrap();
    files_changed(cx, &o);
    assert_eq!(banner(cx, &o).as_deref(), Some("Deleted on disk"));
    fs::write(&file, "SELECT 1").unwrap();
    files_changed(cx, &o);
    assert!(
        !present(cx, o.win, "disk-banner"),
        "the file is back as it was"
    );
    assert_eq!(tab_sql(cx, &o), "SELECT 1");
}

#[gpui_kit::test]
fn close_on_a_deleted_file_closes_the_tab(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let file = open_saved(cx, &o, "lite", "SELECT 1");
    let before = o.shell.read_with(cx, |s, _| s.tabs().len());
    fs::remove_file(&file).unwrap();
    files_changed(cx, &o);
    click_id(cx, o.win, "disk-close");
    assert_eq!(
        o.shell.read_with(cx, |s, _| s.tabs().len()),
        before - 1,
        "closed unasked when clean"
    );
}

#[gpui_kit::test]
fn an_unreadable_file_says_why(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let file = open_saved(cx, &o, "lite", "SELECT 1");
    fs::remove_file(&file).unwrap();
    fs::create_dir(&file).unwrap();
    files_changed(cx, &o);
    let text = banner(cx, &o).unwrap();
    assert!(text.starts_with("Cannot read the file on disk: "), "{text}");
    click_id(cx, o.win, "disk-keep");
    files_changed(cx, &o);
    assert!(
        !present(cx, o.win, "disk-banner"),
        "the same failure stays hidden"
    );
}

#[gpui_kit::test]
fn a_rename_during_a_reload_reads_as_stale(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    open_saved(cx, &o, "lite", "SELECT 1");
    let dir = o.fx.dir.path().to_path_buf();
    let (shell, tab) = (
        o.shell.clone(),
        o.shell.read_with(cx, |s, _| s.tab().unwrap().clone()),
    );
    // The pass snapshots the old path, then the app moves the folder and rebases the tab
    // before the background read runs, as `finish_connection_write` does.
    cx.update_window(o.win, |_, w, cx| {
        shell.update(cx, |s, cx| s.workspace_files_changed(w, cx));
        fs::rename(dir.join("queries/lite"), dir.join("queries/lite2")).unwrap();
        fs::write(dir.join("queries/lite2/a.sql"), "SELECT 2").unwrap();
        tab.update(cx, |t, cx| {
            t.rebase_file(Path::new("queries/lite"), Path::new("queries/lite2"), cx)
        });
    })
    .unwrap();
    settle_reload(cx, &o.shell);
    assert!(
        !present(cx, o.win, "disk-banner"),
        "a moved tab is not a deleted one"
    );
    assert_eq!(tab_sql(cx, &o), "SELECT 2", "the stale pass reads again");
}

#[gpui_kit::test]
fn a_renamed_connection_carries_its_open_tabs(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    open_saved(cx, &o, "nopg", "SELECT 1");
    let tab = o.shell.read_with(cx, |s, _| s.tab().unwrap().clone());
    let shell = o.shell.clone();
    cx.update_window(o.win, |_, w, cx| {
        shell.update(cx, |s, cx| s.switch_tab_connection(&tab, "lite", w, cx))
    })
    .unwrap();
    crate::support::gear(cx, o.win, "nopg");
    crate::support::fill(cx, &o, dbento_app::connection_dialog::Field::Name, "pg2");
    click_id(cx, o.win, "conn-save");
    let dir = o.fx.dir.path().to_path_buf();
    wait_real(cx, "the moved folder", |_| {
        dir.join("queries/pg2/a.sql").exists()
    });
    let file = tab.read_with(cx, |t, _| t.file().map(PathBuf::from));
    assert_eq!(file, Some(PathBuf::from("queries/pg2/a.sql")), "rebased");
    files_changed(cx, &o);
    assert!(
        !present(cx, o.win, "disk-banner"),
        "the own move is no delete"
    );
    let real = o
        .shell
        .read_with(cx, |s, _| s.workspace().unwrap().dir().to_path_buf());
    let state = AppPaths::in_dir(o.files.path()).state;
    wait_real(cx, "the saved session", |_| {
        load_json::<SavedState>(&state)
            .value
            .session(&real)
            .is_some_and(|x| {
                x.tabs
                    .iter()
                    .any(|t| t.file.as_deref() == Some(Path::new("queries/pg2/a.sql")))
            })
    });
}

// Disk text equal to the editor's, as when a teammate made the same edit, is in sync: the
// tab takes it as saved and shows nothing.
#[gpui_kit::test]
fn disk_text_equal_to_the_editor_is_in_sync(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let file = open_saved(cx, &o, "lite", "SELECT 1");
    type_sql(cx, &o, "SELECT 1 -- mine");
    fs::write(&file, "SELECT 1 -- mine").unwrap();
    files_changed(cx, &o);
    assert!(
        !present(cx, o.win, "disk-banner"),
        "the same text is no change"
    );
    let (disk, edited) = o.shell.read_with(cx, |s, cx| {
        let t = s.tab().unwrap().read(cx);
        (t.disk().map(str::to_owned), t.edited(cx))
    });
    assert_eq!(disk.as_deref(), Some("SELECT 1 -- mine"), "disk follows");
    assert!(!edited, "the tab matches its file");
}

#[gpui_kit::test]
fn a_kept_delete_rewritten_with_my_text_is_in_sync(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let file = open_saved(cx, &o, "lite", "SELECT 1");
    type_sql(cx, &o, "SELECT 1 -- mine");
    fs::remove_file(&file).unwrap();
    files_changed(cx, &o);
    click_id(cx, o.win, "disk-keep");
    fs::write(&file, "SELECT 1 -- mine").unwrap();
    files_changed(cx, &o);
    assert!(
        !present(cx, o.win, "disk-banner"),
        "my text written back is no change"
    );
    assert_eq!(tab_sql(cx, &o), "SELECT 1 -- mine");
}

// An edited tab whose file changed outside: the Changed banner shows, then Ctrl+S asks.
fn changed_then_save(cx: &mut TestAppContext) -> (Opened, PathBuf) {
    let o = opened(cx, "", "", 0);
    let file = open_saved(cx, &o, "lite", "SELECT 1");
    type_sql(cx, &o, "SELECT 1 -- mine");
    fs::write(&file, "SELECT 2").unwrap();
    files_changed(cx, &o);
    assert_eq!(
        banner(cx, &o).as_deref(),
        Some("Changed on disk"),
        "the banner shows before the save"
    );
    press_save(cx, &o);
    wait_real(cx, "the conflict question", |cx| {
        present(cx, o.win, "save-conflict-cancel")
    });
    (o, file)
}

#[gpui_kit::test]
fn the_save_conflicts_reload_clears_the_banner(cx: &mut TestAppContext) {
    let (o, _) = changed_then_save(cx);
    click_id(cx, o.win, "save-conflict-reload");
    assert_eq!(tab_sql(cx, &o), "SELECT 2");
    assert!(
        !present(cx, o.win, "disk-banner"),
        "the tab holds the file's text, so nothing is left to report"
    );
}

#[gpui_kit::test]
fn the_save_conflicts_overwrite_clears_the_banner(cx: &mut TestAppContext) {
    let (o, file) = changed_then_save(cx);
    click_id(cx, o.win, "save-conflict-overwrite");
    assert_eq!(fs::read_to_string(&file).unwrap(), "SELECT 1 -- mine");
    assert!(
        !present(cx, o.win, "disk-banner"),
        "the file holds the tab's text, so nothing is left to report"
    );
}

#[gpui_kit::test]
fn unbinding_a_tab_clears_its_banner(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let file = open_saved(cx, &o, "lite", "SELECT 1");
    fs::remove_file(&file).unwrap();
    files_changed(cx, &o);
    assert_eq!(banner(cx, &o).as_deref(), Some("Deleted on disk"));
    let tab = o.shell.read_with(cx, |s, _| s.tab().unwrap().clone());
    tab.update(cx, |t, cx| t.unbind_file(cx));
    cx.run_until_parked();
    assert!(
        !present(cx, o.win, "disk-banner"),
        "a tab with no file has no file to report on"
    );
}
