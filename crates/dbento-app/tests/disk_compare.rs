//! Compare shows the disk text against the tab's as a line diff, with Reload and Keep mine.
use crate::support::{
    Opened, click_id, files_changed, label_of, open_saved, opened, present, tab_sql, wait_real,
};
use dbento_app::workspace_reload::disk_diff;
use gpui_kit::{AppContext as _, TestAppContext};
use std::fs;

#[test]
fn the_diff_marks_disk_lines_minus_and_tab_lines_plus() {
    let d = disk_diff("SELECT 1\nFROM t\n", "SELECT 2\nFROM t\n");
    assert!(d.starts_with("--- disk\n+++ this tab\n"), "{d}");
    assert!(
        d.ends_with("@@ -1,2 +1,2 @@\n-SELECT 1\n+SELECT 2\n FROM t\n"),
        "{d}"
    );
    let bare = disk_diff("SELECT 1", "SELECT 2");
    assert!(
        bare.contains("-SELECT 1") && bare.contains("+SELECT 2"),
        "{bare}"
    );
}

fn changed(cx: &mut TestAppContext) -> Opened {
    let o = opened(cx, "", "", 0);
    let file = open_saved(cx, &o, "lite", "SELECT 1");
    let tab = o.shell.read_with(cx, |s, _| s.tab().unwrap().clone());
    cx.update_window(o.win, |_, w, cx| {
        tab.update(cx, |t, cx| t.set_sql("SELECT 1 -- mine", w, cx))
    })
    .unwrap();
    fs::write(&file, "SELECT 2").unwrap();
    files_changed(cx, &o);
    click_id(cx, o.win, "disk-compare");
    wait_real(cx, "the compare dialog", |cx| {
        present(cx, o.win, "disk-compare-keep")
    });
    o
}

#[gpui_kit::test]
fn compare_shows_the_diff_and_keep_mine_keeps(cx: &mut TestAppContext) {
    let o = changed(cx);
    let shown = label_of(cx, o.win, "confirm-sql").unwrap();
    assert!(
        shown.contains("-SELECT 2") && shown.contains("+SELECT 1 -- mine"),
        "{shown}"
    );
    click_id(cx, o.win, "disk-compare-keep");
    assert_eq!(tab_sql(cx, &o), "SELECT 1 -- mine");
    assert!(!present(cx, o.win, "disk-banner"), "kept");
    let disk = o.shell.read_with(cx, |s, cx| {
        s.tab().unwrap().read(cx).disk().map(str::to_owned)
    });
    assert_eq!(
        disk.as_deref(),
        Some("SELECT 2"),
        "the next save overwrites unasked"
    );
}

#[gpui_kit::test]
fn compare_reload_takes_the_disk_text(cx: &mut TestAppContext) {
    let o = changed(cx);
    click_id(cx, o.win, "disk-compare-reload");
    assert_eq!(tab_sql(cx, &o), "SELECT 2");
    assert!(!present(cx, o.win, "disk-banner"), "reloaded");
}

#[gpui_kit::test]
fn compare_cancel_changes_nothing(cx: &mut TestAppContext) {
    let o = changed(cx);
    click_id(cx, o.win, "disk-compare-cancel");
    assert_eq!(tab_sql(cx, &o), "SELECT 1 -- mine");
    assert!(present(cx, o.win, "disk-banner"), "the banner stays");
}

#[gpui_kit::test]
fn an_answer_after_the_file_moved_on_does_nothing(cx: &mut TestAppContext) {
    let o = changed(cx);
    fs::write(o.fx.dir.path().join("queries/lite/a.sql"), "SELECT 3").unwrap();
    files_changed(cx, &o);
    click_id(cx, o.win, "disk-compare-reload");
    assert_eq!(
        tab_sql(cx, &o),
        "SELECT 1 -- mine",
        "compared SELECT 2, not SELECT 3"
    );
    assert!(
        present(cx, o.win, "disk-banner"),
        "the newer change still shows"
    );
}
