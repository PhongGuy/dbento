//! Tabs and edited SQL across a crash, a window close and a workspace switch.
use crate::support;
use crate::support::{Opened, label_of, opened, present, wait_real};
use dbento_app::app::{AppPaths, SavedState, start};
use dbento_app::drafts::{DRAFT_DEBOUNCE, Draft, Drafts};
use dbento_app::persist::load_json;
use dbento_app::shell::Shell;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AnyWindowHandle, AppContext as _, Entity, TestAppContext, VisualTestContext};
use std::path::{Path, PathBuf};

fn settle(cx: &mut TestAppContext) {
    cx.executor().advance_clock(DRAFT_DEBOUNCE);
    cx.run_until_parked();
}
fn drafts(o: &Opened) -> Vec<Draft> {
    Drafts::new(AppPaths::in_dir(o.files.path()).drafts).load(o.fx.ws.dir())
}
fn set(cx: &mut TestAppContext, win: AnyWindowHandle, shell: &Entity<Shell>, ix: usize, sql: &str) {
    let tab = shell.read_with(cx, |s, _| s.tabs()[ix].clone());
    cx.update_window(win, |_, w, cx| {
        tab.update(cx, |t, cx| t.set_sql(sql, w, cx))
    })
    .unwrap();
    cx.run_until_parked();
}
fn click(cx: &mut TestAppContext, win: AnyWindowHandle, id: &'static str) {
    cx.update_window(win, |_, w, cx| w.click(id, cx)).unwrap();
    cx.run_until_parked();
}
/// A second start on the same files and workspace: what the next launch sees.
fn restart(cx: &mut TestAppContext, o: &Opened) -> (AnyWindowHandle, Entity<Shell>) {
    let (win, shell) = cx
        .update(|cx| {
            start(
                support::services(o.files.path()),
                Some(o.fx.dir.path().into()),
                cx,
            )
        })
        .unwrap();
    assert!(present(cx, win, "connection-list"));
    (win, shell)
}
/// Per tab: connection, SQL, edited, file.
fn tabs(
    cx: &mut TestAppContext,
    shell: &Entity<Shell>,
) -> Vec<(String, String, bool, Option<PathBuf>)> {
    shell.read_with(cx, |s, cx| {
        s.tabs()
            .iter()
            .map(|t| {
                let t = t.read(cx);
                (
                    t.model().read(cx).spec().connection.clone(),
                    t.sql(cx),
                    t.edited(cx),
                    t.file().map(Path::to_path_buf),
                )
            })
            .collect()
    })
}
fn closed(cx: &mut TestAppContext) -> bool {
    cx.update(|cx| cx.windows().is_empty())
}

#[gpui_kit::test]
fn a_crash_keeps_edited_sql_and_the_next_start_restores_it(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    click(cx, o.win, "tab-new");
    set(cx, o.win, &o.shell, 0, "SELECT 42");
    settle(cx);
    wait_real(cx, "the draft", |_| drafts(&o).len() == 1);
    // No close: the first window is simply abandoned, as a crash would leave it.
    let (_, shell) = restart(cx, &o);
    assert_eq!(
        tabs(cx, &shell),
        [
            ("lite".into(), "SELECT 42".into(), true, None),
            ("lite".into(), String::new(), false, None)
        ]
    );
    let models: Vec<_> = shell.read_with(cx, |s, cx| {
        s.tabs()
            .iter()
            .map(|t| t.read(cx).model().clone())
            .collect()
    });
    assert_eq!(shell.read_with(cx, |s, _| s.active()), Some(1));
    assert!(
        models
            .iter()
            .all(|m| m.read_with(cx, |m, _| m.run_id() == 0)),
        "restore runs nothing"
    );
    assert!(
        models[0].read_with(cx, |m, _| m.events().is_none()),
        "an inactive restored tab has not connected"
    );
    assert!(
        models[1].read_with(cx, |m, _| m.events().is_some()),
        "the active one has"
    );
}

#[gpui_kit::test]
fn a_restored_saved_query_keeps_its_file_and_its_unsaved_edit(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let file = o.fx.dir.path().join("queries/lite/a.sql");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "SELECT 7").unwrap();
    let q = o.fx.ws.saved_queries().unwrap().into_iter().next().unwrap();
    let shell = o.shell.clone();
    cx.update_window(o.win, |_, w, cx| {
        shell.update(cx, |s, cx| s.open_saved(&q, w, cx))
    })
    .unwrap();
    set(cx, o.win, &o.shell, 1, "SELECT 8");
    settle(cx);
    wait_real(cx, "the draft", |_| drafts(&o).len() == 1);
    let (win, shell) = restart(cx, &o);
    let restored = tabs(cx, &shell);
    assert_eq!(
        restored[1],
        (
            "lite".into(),
            "SELECT 8".into(),
            true,
            Some("queries/lite/a.sql".into())
        )
    );
    set(cx, win, &shell, 1, "SELECT 7");
    assert!(!tabs(cx, &shell)[1].2, "the baseline is the file's text");
}

#[gpui_kit::test]
fn closing_the_window_with_edited_sql_keeps_it_without_asking(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    click(cx, o.win, "tab-new");
    set(cx, o.win, &o.shell, 0, "SELECT 42"); // no pause: the close itself must write the draft
    let mut v = VisualTestContext::from_window(o.win, cx);
    assert!(!v.simulate_close(), "the close waits for the sessions");
    // SQLite sessions may close the window within the same tick; a question would hold it.
    let asked = cx.update_window(o.win, |_, w, cx| {
        w.render_frame(cx);
        w.try_find("confirm-dialog").is_some()
    });
    assert!(!asked.unwrap_or(false));
    wait_real(cx, "the window to close", closed);
    let (_, shell) = restart(cx, &o);
    assert_eq!(tabs(cx, &shell)[0].1, "SELECT 42");
}

#[gpui_kit::test]
fn a_draft_that_cannot_be_written_holds_the_close_with_a_question(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    std::fs::write(AppPaths::in_dir(o.files.path()).drafts, "").unwrap(); // a file where the folder goes
    set(cx, o.win, &o.shell, 0, "SELECT 42");
    let mut v = VisualTestContext::from_window(o.win, cx);
    assert!(!v.simulate_close());
    wait_real(cx, "the dialog", |cx| present(cx, o.win, "confirm-dialog"));
    assert_eq!(
        label_of(cx, o.win, "confirm-detail").as_deref(),
        Some("1 tab could not be kept as a draft.")
    );
    click(cx, o.win, "discard-keep");
    assert_eq!(tabs(cx, &o.shell)[0].1, "SELECT 42");
    let mut v = VisualTestContext::from_window(o.win, cx);
    assert!(!v.simulate_close());
    wait_real(cx, "the dialog again", |cx| {
        present(cx, o.win, "confirm-dialog")
    });
    click(cx, o.win, "discard-close");
    wait_real(cx, "the window to close", closed);
}

#[gpui_kit::test]
fn a_draft_no_session_names_is_restored_as_an_extra_tab(cx: &mut TestAppContext) {
    support::init(cx);
    support::init_tokio(cx);
    let (fx, files) = (support::fixture("", ""), tempfile::tempdir().unwrap());
    let store = Drafts::new(AppPaths::in_dir(files.path()).drafts);
    let orphan = Draft::new(
        &Drafts::new_id(),
        fx.ws.dir(),
        "lite",
        None,
        "SELECT orphan",
    );
    let gone = Draft::new(&Drafts::new_id(), fx.ws.dir(), "gone", None, "SELECT gone");
    store.write(&orphan).unwrap();
    store.write(&gone).unwrap();
    let (win, shell) = cx
        .update(|cx| {
            start(
                support::services(files.path()),
                Some(fx.dir.path().into()),
                cx,
            )
        })
        .unwrap();
    assert!(present(cx, win, "connection-list"));
    assert_eq!(
        tabs(cx, &shell),
        [("lite".into(), "SELECT orphan".into(), true, None)]
    );
    assert!(
        store.path_of(&gone.id).unwrap().exists(),
        "a draft on a gone connection stays on disk"
    );
}

#[gpui_kit::test]
fn switching_workspace_keeps_edited_sql_and_switching_back_restores_it(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    set(cx, o.win, &o.shell, 0, "SELECT 42");
    let other = support::fixture("", "");
    let switch = |cx: &mut TestAppContext, dir: PathBuf| {
        let shell = o.shell.clone();
        cx.update_window(o.win, |_, w, cx| {
            shell.update(cx, |s, cx| s.switch_workspace(dir, w, cx))
        })
        .unwrap();
        cx.run_until_parked();
    };
    switch(cx, other.dir.path().into());
    assert!(!present(cx, o.win, "confirm-dialog"));
    assert!(tabs(cx, &o.shell).is_empty());
    switch(cx, o.fx.dir.path().into());
    assert_eq!(
        tabs(cx, &o.shell),
        [("lite".into(), "SELECT 42".into(), true, None)]
    );
}

#[gpui_kit::test]
fn closing_an_unedited_tab_drops_it_from_the_session(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    click(cx, o.win, "tab-new");
    cx.update_window(o.win, |_, w, cx| w.press("ctrl-w", cx))
        .unwrap();
    cx.run_until_parked();
    let saved: SavedState = load_json(&AppPaths::in_dir(o.files.path()).state).value;
    assert_eq!(saved.session(o.fx.ws.dir()).unwrap().tabs.len(), 1);
    let (_, shell) = restart(cx, &o);
    assert_eq!(tabs(cx, &shell).len(), 1);
}

// Starts a long run and a window close, which waits on the run's session.
fn closing(cx: &mut TestAppContext) -> Opened {
    let o = opened(cx, "", "", 0);
    o.run(cx, support::LONG);
    let tab = o.shell.read_with(cx, |s, _| s.tab().unwrap().clone());
    cx.update(|cx| tab.update(cx, |t, cx| t.mark_saved(cx)));
    // The run would raise the quit question (tests/quit_ask.rs); close as its Close answer does.
    let shell = o.shell.clone();
    cx.update_window(o.win, |_, w, cx| {
        shell.update(cx, |s, cx| s.close_unasked(w, cx))
    })
    .unwrap();
    o
}
fn saved_tabs(o: &Opened) -> (Option<PathBuf>, usize) {
    let saved: SavedState = load_json(&AppPaths::in_dir(o.files.path()).state).value;
    let n = saved.session(o.fx.ws.dir()).map_or(0, |s| s.tabs.len());
    (saved.last_workspace, n)
}

#[gpui_kit::test]
fn no_workspace_switch_while_the_window_is_closing(cx: &mut TestAppContext) {
    let o = closing(cx);
    let before = saved_tabs(&o);
    assert_eq!(before.1, 1);
    let other = support::fixture("", "");
    let shell = o.shell.clone();
    cx.update_window(o.win, |_, w, cx| {
        shell.update(cx, |s, cx| {
            s.switch_workspace(other.dir.path().into(), w, cx)
        })
    })
    .unwrap();
    assert_eq!(saved_tabs(&o), before);
    wait_real(cx, "the window to close", closed);
}

#[gpui_kit::test]
fn a_restore_that_cannot_reopen_its_tabs_keeps_the_saved_session(cx: &mut TestAppContext) {
    let o = closing(cx);
    let dir: PathBuf = o.fx.dir.path().into();
    let shell = o.shell.clone();
    cx.update_window(o.win, |_, w, cx| {
        shell.update(cx, |s, cx| s.open_workspace(dir, w, cx))
    })
    .unwrap();
    assert_eq!(saved_tabs(&o).1, 1);
    wait_real(cx, "the window to close", closed);
}

#[gpui_kit::test]
fn restored_untitled_tabs_are_numbered_in_restore_order(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    click(cx, o.win, "tab-new");
    click(cx, o.win, "tab-new");
    set(cx, o.win, &o.shell, 0, "SELECT 1");
    set(cx, o.win, &o.shell, 2, "SELECT 3");
    settle(cx);
    wait_real(cx, "the drafts", |_| drafts(&o).len() == 2);
    let (win, _) = restart(cx, &o);
    let labels: Vec<_> = (0usize..)
        .map_while(|i| label_of(cx, win, ("tab", i)))
        .collect();
    assert_eq!(labels, ["Query 1", "Query 2", "Query 3"]);
}

// Opens a value tab on key `k` of DB 0, as the explorer's OpenKey does; it becomes active.
fn open_key_tab(cx: &mut TestAppContext, win: AnyWindowHandle, shell: &Entity<Shell>) {
    cx.update_window(win, |_, w, cx| {
        shell.update(cx, |s, cx| s.open_key(0, 0, b"k".to_vec(), w, cx))
    })
    .unwrap();
    cx.run_until_parked();
}

#[gpui_kit::test]
fn a_click_on_the_connection_returns_to_its_query_tab_past_a_newer_key_tab(
    cx: &mut TestAppContext,
) {
    let o = opened(cx, "", "", 0);
    let query = o.shell.read_with(cx, |s, _| s.active());
    open_key_tab(cx, o.win, &o.shell);
    assert_ne!(o.shell.read_with(cx, |s, _| s.active()), query);

    // The key tab hides its editor, so the connection's query tab is the one to return to.
    cx.update_window(o.win, |_, w, cx| {
        o.shell.update(cx, |s, cx| s.click_connection(0, w, cx))
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(o.shell.read_with(cx, |s, _| s.active()), query);
}

#[gpui_kit::test]
fn a_key_tab_is_left_out_of_the_session_and_the_active_tab_still_maps(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    click(cx, o.win, "tab-new");
    set(cx, o.win, &o.shell, 0, "SELECT 1");
    settle(cx);
    wait_real(cx, "the draft", |_| drafts(&o).len() == 1);
    let query_tabs = |cx: &mut TestAppContext, shell: &Entity<Shell>| {
        tabs(cx, shell).into_iter().map(|t| t.1).collect::<Vec<_>>()
    };

    // The key tab is the active one: the last query tab before it takes its place.
    open_key_tab(cx, o.win, &o.shell);
    assert_eq!(o.shell.read_with(cx, |s, _| s.active()), Some(2));
    settle(cx);
    let (win, shell) = restart(cx, &o);
    assert_eq!(query_tabs(cx, &shell), ["SELECT 1", ""]);
    assert_eq!(shell.read_with(cx, |s, _| s.active()), Some(1));

    // The key tab sits after the active one, which keeps its index.
    open_key_tab(cx, win, &shell);
    cx.update_window(win, |_, w, cx| {
        shell.update(cx, |s, cx| s.activate(0, w, cx))
    })
    .unwrap();
    settle(cx);
    let (_, shell) = restart(cx, &o);
    assert_eq!(query_tabs(cx, &shell), ["SELECT 1", ""]);
    assert_eq!(shell.read_with(cx, |s, _| s.active()), Some(0));
}
