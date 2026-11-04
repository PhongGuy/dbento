//! Saved queries in the app: Ctrl+O lists and opens `queries/*.sql`, Ctrl+S names and saves.
mod support;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, ElementId, Focusable as _, TestAppContext};
use std::fs;
use std::path::Path;
use support::{Opened, label_of, opened, palette_pick, palette_rows, present, wait_real};

fn click(cx: &mut TestAppContext, o: &Opened, id: impl Into<ElementId>) {
    let id = id.into();
    cx.update_window(o.win, |_, w, cx| w.click(id, cx)).unwrap();
    cx.run_until_parked();
}
/// Sets the active tab's SQL, focuses its editor and presses `key`.
fn press_in_editor(cx: &mut TestAppContext, o: &Opened, sql: &str, key: &str) {
    let tab = o.shell.read_with(cx, |s, _| s.tab().unwrap().clone());
    let e = o
        .shell
        .read_with(cx, |s, cx| s.tab().unwrap().read(cx).editor().clone());
    cx.update_window(o.win, |_, w, cx| {
        tab.update(cx, |t, cx| t.set_sql(sql, w, cx));
        e.update(cx, |e, cx| e.focus(w, cx));
        w.press(key, cx);
    })
    .unwrap();
    cx.run_until_parked();
}
fn name(cx: &mut TestAppContext, o: &Opened, text: &str) {
    wait_real(cx, "the name dialog", |cx| present(cx, o.win, "save-name"));
    cx.update_window(o.win, |_, w, cx| {
        w.press("ctrl-a", cx);
        w.input(text, cx);
    })
    .unwrap();
    click(cx, o, "save-ok");
}
fn active(cx: &mut TestAppContext, o: &Opened) -> (usize, Option<usize>, String, String, bool) {
    o.shell.read_with(cx, |s, cx| {
        let t = s.tab().unwrap().read(cx);
        let conn = t.model().read(cx).spec().connection.clone();
        (s.tabs().len(), s.active(), t.sql(cx), conn, t.edited(cx))
    })
}

#[gpui_kit::test]
fn ctrl_s_names_an_unsaved_tab_then_rewrites_its_file(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    press_in_editor(cx, &o, "SELECT 1", "ctrl-s");
    name(cx, &o, "top");
    let file = o.fx.dir.path().join("queries/lite/top.sql");
    wait_real(cx, "the file", |_| file.exists());
    assert_eq!(fs::read_to_string(&file).unwrap(), "SELECT 1");
    assert_eq!(
        label_of(cx, o.win, ("tab", 0usize)).as_deref(),
        Some("top.sql")
    );
    assert!(!active(cx, &o).4, "a saved tab is not edited");
    press_in_editor(cx, &o, "SELECT 2", "ctrl-s");
    assert!(!present(cx, o.win, "save-name"));
    assert_eq!(fs::read_to_string(&file).unwrap(), "SELECT 2");
}

#[gpui_kit::test]
fn a_name_that_leaves_queries_is_refused(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    press_in_editor(cx, &o, "SELECT 1", "ctrl-s");
    name(cx, &o, "../../escape");
    wait_real(cx, "the error", |cx| present(cx, o.win, "save-error-ok"));
    let dir = o.fx.dir.path();
    assert!(!dir.join("escape.sql").exists() && !dir.join("queries/escape.sql").exists());
    click(cx, &o, "save-error-ok");
    assert_eq!(
        label_of(cx, o.win, ("tab", 0usize)).as_deref(),
        Some("Query 1")
    );
    assert!(active(cx, &o).4, "the tab is still edited");
}

#[gpui_kit::test]
fn saving_over_an_existing_name_asks_first(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let file = o.fx.dir.path().join("queries/lite/top.sql");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, "SELECT 0").unwrap();
    press_in_editor(cx, &o, "SELECT 1", "ctrl-s");
    name(cx, &o, "top");
    wait_real(cx, "the question", |cx| {
        present(cx, o.win, "overwrite-keep")
    });
    click(cx, &o, "overwrite-keep");
    assert_eq!(fs::read_to_string(&file).unwrap(), "SELECT 0");
    assert_eq!(
        label_of(cx, o.win, ("tab", 0usize)).as_deref(),
        Some("Query 1")
    );
    assert!(active(cx, &o).4, "a kept file leaves the tab edited");
    press_in_editor(cx, &o, "SELECT 1", "ctrl-s");
    name(cx, &o, "top.sql");
    wait_real(cx, "the question", |cx| {
        present(cx, o.win, "overwrite-replace")
    });
    click(cx, &o, "overwrite-replace");
    assert_eq!(fs::read_to_string(&file).unwrap(), "SELECT 1");
    assert_eq!(
        label_of(cx, o.win, ("tab", 0usize)).as_deref(),
        Some("top.sql")
    );
    assert!(!active(cx, &o).4, "a saved tab is not edited");
}

#[gpui_kit::test]
fn the_save_button_saves_like_ctrl_s(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let tab = o.shell.read_with(cx, |s, _| s.tab().unwrap().clone());
    cx.update_window(o.win, |_, w, cx| {
        tab.update(cx, |t, cx| t.set_sql("SELECT 3", w, cx))
    })
    .unwrap();
    click(cx, &o, "query-save");
    name(cx, &o, "button");
    let file = o.fx.dir.path().join("queries/lite/button.sql");
    assert_eq!(fs::read_to_string(&file).unwrap(), "SELECT 3");
    assert!(!active(cx, &o).4, "a saved tab is not edited");
}

#[gpui_kit::test]
fn an_unlisted_file_behind_a_folder_link_is_not_overwritten_silently(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let dir = o.fx.dir.path();
    fs::create_dir_all(dir.join("queries/real")).unwrap();
    fs::write(dir.join("queries/real/top.sql"), "SELECT 0").unwrap();
    std::os::unix::fs::symlink(dir.join("queries/real"), dir.join("queries/lite")).unwrap();
    press_in_editor(cx, &o, "SELECT 1", "ctrl-s");
    name(cx, &o, "top");
    wait_real(cx, "the question", |cx| {
        present(cx, o.win, "overwrite-keep")
    });
    click(cx, &o, "overwrite-keep");
    assert_eq!(
        fs::read_to_string(dir.join("queries/real/top.sql")).unwrap(),
        "SELECT 0"
    );
}

#[gpui_kit::test]
fn an_upper_case_suffix_is_kept(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    press_in_editor(cx, &o, "SELECT 1", "ctrl-s");
    name(cx, &o, "x.SQL");
    let file = o.fx.dir.path().join("queries/lite/x.SQL");
    wait_real(cx, "the file", |_| file.exists());
    assert!(!o.fx.dir.path().join("queries/lite/x.SQL.sql").exists());
    assert_eq!(
        label_of(cx, o.win, ("tab", 0usize)).as_deref(),
        Some("x.SQL")
    );
}

#[gpui_kit::test]
fn an_empty_or_bare_suffix_name_is_refused_in_the_dialog(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    press_in_editor(cx, &o, "SELECT 1", "ctrl-s");
    for (bad, says) in [
        ("  ", "Enter a name."),
        (".SQL", "Enter a name before .sql."),
    ] {
        name(cx, &o, bad);
        assert!(present(cx, o.win, "save-name"), "the dialog stays open");
        assert_eq!(
            label_of(cx, o.win, "save-name-error").as_deref(),
            Some(says)
        );
    }
    name(cx, &o, "fine");
    let file = o.fx.dir.path().join("queries/lite/fine.sql");
    wait_real(cx, "the file", |_| file.exists());
    assert!(!present(cx, o.win, "save-name"));
    assert!(!o.fx.dir.path().join("queries/lite/.SQL").exists());
}

fn ctrl_o(cx: &mut TestAppContext, o: &Opened) {
    let e = o
        .shell
        .read_with(cx, |s, cx| s.tab().unwrap().read(cx).editor().clone());
    cx.update_window(o.win, |_, w, cx| {
        e.update(cx, |e, cx| e.focus(w, cx));
        w.press("ctrl-o", cx);
    })
    .unwrap();
    cx.run_until_parked();
}

#[gpui_kit::test]
fn ctrl_o_lists_saved_queries_and_opens_each_once(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let dir = o.fx.dir.path();
    fs::create_dir_all(dir.join("queries/lite")).unwrap();
    fs::write(dir.join("queries/lite/a.sql"), "SELECT 7").unwrap();
    fs::write(dir.join("queries/misc.sql"), "SELECT 8").unwrap();
    ctrl_o(cx, &o);
    wait_real(cx, "the list", |cx| palette_rows(cx, o.win).len() == 2);
    assert_eq!(
        palette_rows(cx, o.win),
        ["queries/lite/a.sql", "queries/misc.sql"]
    );
    palette_pick(cx, o.win, "queries/lite/a.sql");
    assert_eq!(
        active(cx, &o),
        (2, Some(1), "SELECT 7".into(), "lite".into(), false)
    );
    assert_eq!(
        label_of(cx, o.win, ("tab", 1usize)).as_deref(),
        Some("a.sql")
    );
    ctrl_o(cx, &o);
    palette_pick(cx, o.win, "queries/lite/a.sql");
    assert_eq!(
        active(cx, &o).0,
        2,
        "an open file is activated, not opened twice"
    );
    click(cx, &o, ("tab", 0usize));
    ctrl_o(cx, &o);
    palette_pick(cx, o.win, "queries/misc.sql");
    assert_eq!(
        active(cx, &o),
        (3, Some(2), "SELECT 8".into(), "lite".into(), false)
    );
}

#[gpui_kit::test]
fn ctrl_o_in_the_grid_opens_the_queries_mode(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let dir = o.fx.dir.path();
    fs::create_dir_all(dir.join("queries")).unwrap();
    fs::write(dir.join("queries/misc.sql"), "SELECT 8").unwrap();
    o.run(cx, "SELECT 1 AS a");
    o.idle(cx);
    let table = o.shell.read_with(cx, |s, cx| {
        s.tab()
            .unwrap()
            .read(cx)
            .results_pane()
            .read(cx)
            .table()
            .clone()
    });
    cx.update_window(o.win, |_, w, cx| {
        table.update(cx, |t, cx| t.set_selected_cell(0, 1, cx));
        table.read(cx).focus_handle(cx).focus(w, cx);
        w.press("ctrl-o", cx);
    })
    .unwrap();
    wait_real(cx, "the list", |cx| {
        palette_rows(cx, o.win) == ["queries/misc.sql"]
    });
}

#[gpui_kit::test]
fn an_empty_queries_folder_says_so(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    ctrl_o(cx, &o);
    wait_real(cx, "the empty note", |cx| {
        present(cx, o.win, "palette-empty")
    });
}

#[gpui_kit::test]
fn replacing_a_file_open_in_another_tab_unbinds_that_tab(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let file = o.fx.dir.path().join("queries/lite/a.sql");
    fs::create_dir_all(file.parent().unwrap()).unwrap();
    fs::write(&file, "SELECT 7").unwrap();
    ctrl_o(cx, &o);
    palette_pick(cx, o.win, "queries/lite/a.sql"); // tab 1 is bound to a.sql
    click(cx, &o, "tab-new"); // tab 2
    press_in_editor(cx, &o, "SELECT 2", "ctrl-s");
    name(cx, &o, "a");
    wait_real(cx, "the question", |cx| {
        present(cx, o.win, "overwrite-replace")
    });
    click(cx, &o, "overwrite-replace");
    assert_eq!(fs::read_to_string(&file).unwrap(), "SELECT 2");
    let tabs: Vec<_> = o.shell.read_with(cx, |s, cx| {
        s.tabs()
            .iter()
            .map(|t| {
                let t = t.read(cx);
                (t.file().map(Path::to_path_buf), t.sql(cx), t.edited(cx))
            })
            .collect()
    });
    assert_eq!(tabs[1], (None, "SELECT 7".to_owned(), true));
    assert_eq!(tabs[2].0.as_deref(), Some(Path::new("queries/lite/a.sql")));
    assert_eq!(
        label_of(cx, o.win, ("tab", 1usize)).as_deref(),
        Some("Query 3")
    );
}
