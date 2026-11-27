//! Opening another workspace from inside the app, and closing back to the welcome view.
use crate::support;
use crate::support::{
    Opened, click_id, expanded_of, label_of, launched, opened, present, wait_real,
};
use dbento_app::app::{AppPaths, SavedState, start};
use dbento_app::keymap::{
    self, CLOSE_WORKSPACE_KEYS, CloseWorkspace, OPEN_WORKSPACE_KEYS, OpenWorkspace, Shortcut,
};
use dbento_app::palette::{Mode, PaletteCommand};
use dbento_app::persist::load_json;
use dbento_app::schema_tree::NodeKey;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext, VisualTestContext};
use std::path::{Path, PathBuf};

fn saved(files: &Path) -> SavedState {
    load_json(&AppPaths::in_dir(files).state).value
}
fn real(dir: &Path) -> PathBuf {
    std::fs::canonicalize(dir).unwrap()
}
fn current(cx: &mut TestAppContext, o: &Opened) -> Option<PathBuf> {
    o.shell
        .read_with(cx, |s, _| s.workspace().map(|w| w.dir().to_path_buf()))
}
fn switch(cx: &mut TestAppContext, o: &Opened, dir: PathBuf) {
    let shell = o.shell.clone();
    cx.update_window(o.win, |_, w, cx| {
        shell.update(cx, |s, cx| s.switch_workspace(dir, w, cx))
    })
    .unwrap();
    cx.run_until_parked();
}
fn close_workspace(cx: &mut TestAppContext, o: &Opened) {
    let shell = o.shell.clone();
    cx.update_window(o.win, |_, w, cx| {
        shell.update(cx, |s, cx| s.close_workspace(w, cx))
    })
    .unwrap();
    cx.run_until_parked();
}
fn command(cx: &mut TestAppContext, o: &Opened, text: &str, row: &str) {
    let shell = o.shell.clone();
    cx.update_window(o.win, |_, w, cx| {
        shell.update(cx, |s, cx| s.open_palette(Mode::Commands, w, cx))
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(o.win, |_, w, cx| w.input(text, cx))
        .unwrap();
    support::palette_pick(cx, o.win, row);
    cx.run_until_parked();
}
fn stage_cell(cx: &mut TestAppContext, o: &Opened) {
    o.open_relation(cx, "t", dbento_core::RelationKind::Table);
    o.wait_browse(cx, "SELECT *\nFROM main.t\nORDER BY id ASC");
    let m = o.model(cx);
    assert!(
        m.update(cx, |m, cx| m.stage_cell(0, 1, "z", cx)),
        "the cell stages"
    );
    cx.run_until_parked();
}
const T: &str = "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT); INSERT INTO t VALUES (1, 'a');";

#[gpui_kit::test]
fn the_workspace_commands_are_in_the_palette_and_the_shortcut_list(cx: &mut TestAppContext) {
    let o = launched(cx, "", "");
    assert_eq!(
        PaletteCommand::OpenWorkspace.label(),
        "Open workspace\u{2026}"
    );
    assert_eq!(PaletteCommand::CloseWorkspace.label(), "Close workspace");
    assert!(PaletteCommand::ALL.contains(&PaletteCommand::OpenWorkspace));
    assert!(PaletteCommand::ALL.contains(&PaletteCommand::CloseWorkspace));
    assert_eq!(
        cx.update(|cx| keymap::keys_for(&OpenWorkspace, cx)),
        Some(OPEN_WORKSPACE_KEYS.into())
    );
    assert_eq!(
        cx.update(|cx| keymap::keys_for(&CloseWorkspace, cx)),
        Some(CLOSE_WORKSPACE_KEYS.into())
    );
    let list = cx.update(|cx| keymap::shortcuts(cx));
    for (label, keys) in [
        ("Open workspace\u{2026}", OPEN_WORKSPACE_KEYS),
        ("Close workspace", CLOSE_WORKSPACE_KEYS),
    ] {
        let want = Shortcut {
            label,
            keys: keys.into(),
        };
        assert!(list.contains(&want), "missing {want:?}");
    }
    assert_eq!(
        keymap::default_keys("workspace.open"),
        Some(OPEN_WORKSPACE_KEYS)
    );
    assert_eq!(
        keymap::default_keys("workspace.close"),
        Some(CLOSE_WORKSPACE_KEYS)
    );
    drop(o);
}

#[gpui_kit::test]
fn open_workspace_from_the_palette_asks_for_a_folder_and_swaps_to_it(cx: &mut TestAppContext) {
    let o = launched(cx, "", "");
    let other = support::fixture("", "");
    command(cx, &o, "open workspace", "Open workspace\u{2026}");
    assert!(cx.did_prompt_for_paths());
    let dir = other.dir.path().to_path_buf();
    cx.simulate_path_prompt_response(move |_| Some(vec![dir.clone()]));
    cx.run_until_parked();
    assert_eq!(current(cx, &o), Some(real(other.dir.path())));
    assert!(present(cx, o.win, "connection-list"));
}

#[gpui_kit::test]
fn the_title_pill_lists_the_other_recent_workspaces_and_opens_one(cx: &mut TestAppContext) {
    let o = launched(cx, "", "");
    let other = support::fixture("", "");
    switch(cx, &o, other.dir.path().into());
    switch(cx, &o, o.fx.dir.path().into());
    assert_eq!(
        label_of(cx, o.win, "title-workspace").as_deref(),
        Some(real(o.fx.dir.path()).file_name().unwrap().to_str().unwrap())
    );
    // At the bar's left, clear of the centred search pill and the window controls.
    let (pill, search) = cx
        .update_window(o.win, |_, w, cx| {
            w.render_frame(cx);
            (
                w.find("title-workspace").bounds(),
                w.find("title-search").bounds(),
            )
        })
        .unwrap();
    assert!(pill.right() <= search.left(), "{pill:?} {search:?}");
    click_id(cx, o.win, "title-workspace");
    // The current folder is not offered again: the only recent row is the other one.
    let other_real = real(other.dir.path());
    assert_eq!(
        label_of(cx, o.win, ("title-workspace-recent", 0usize)).as_deref(),
        Some(other_real.to_str().unwrap())
    );
    assert!(!present(cx, o.win, ("title-workspace-recent", 1usize)));
    assert!(present(cx, o.win, "title-workspace-open"));
    assert!(present(cx, o.win, "title-workspace-close"));
    click_id(cx, o.win, ("title-workspace-recent", 0usize));
    assert_eq!(current(cx, &o), Some(other_real));
}

#[gpui_kit::test]
fn a_swap_closes_the_old_workspaces_catalog_sessions(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let sb = o.shell.read_with(cx, |s, _| s.sidebar().cloned().unwrap());
    sb.update(cx, |s, cx| {
        s.toggle_node(NodeKey::Connection("lite".into()), cx)
    });
    wait_real(cx, "the lite catalog", |cx| {
        sb.read_with(cx, |s, _| s.catalog_sessions() == 1)
    });
    let other = support::fixture("", "");
    switch(cx, &o, other.dir.path().into());
    assert_eq!(current(cx, &o), Some(real(other.dir.path())));
    assert_eq!(sb.read_with(cx, |s, _| s.catalog_sessions()), 0);
    wait_real(cx, "the old sessions to close", |cx| {
        o.shell.read_with(cx, |s, _| s.pending_closings() == 0)
    });
}

#[gpui_kit::test]
fn a_missing_folder_keeps_the_current_workspace_and_says_why(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let other = support::fixture("", "");
    let gone = real(other.dir.path());
    switch(cx, &o, gone.clone());
    switch(cx, &o, o.fx.dir.path().into());
    open_tab(cx, &o);
    let tab = o.shell.read_with(cx, |s, _| s.tab().unwrap().entity_id());
    drop(other);
    switch(cx, &o, gone.clone());
    wait_real(cx, "the error", |cx| present(cx, o.win, "confirm-detail"));
    let detail = label_of(cx, o.win, "confirm-detail").unwrap();
    assert!(detail.contains("dbento.toml"), "{detail}");
    click_id(cx, o.win, "save-error-ok");
    assert_eq!(current(cx, &o), Some(real(o.fx.dir.path())));
    assert_eq!(
        o.shell.read_with(cx, |s, _| s.tab().unwrap().entity_id()),
        tab,
        "the open tab stays"
    );
    // The entry waits, marked, as on the welcome view, for a drive that comes back.
    let s = saved(o.files.path());
    assert!(s.recent_workspaces.contains(&gone));
    assert_eq!(s.last_workspace.as_deref(), Some(o.fx.dir.path()));
    click_id(cx, o.win, "title-workspace");
    assert!(present(cx, o.win, ("title-workspace-missing", 0usize)));
}
fn open_tab(cx: &mut TestAppContext, o: &Opened) {
    if o.shell.read_with(cx, |s, _| s.tab().is_none()) {
        support::open_connection_row(cx, o.win, "lite");
    }
}

#[gpui_kit::test]
fn opening_the_current_folder_changes_nothing(cx: &mut TestAppContext) {
    let o = opened(cx, T, "", 0);
    stage_cell(cx, &o);
    let tab = o.shell.read_with(cx, |s, _| s.tab().unwrap().entity_id());
    switch(cx, &o, o.fx.dir.path().into());
    #[cfg(unix)]
    {
        let link = o.files.path().join("link");
        std::os::unix::fs::symlink(o.fx.dir.path(), &link).unwrap();
        switch(cx, &o, link);
    }
    assert!(!present(cx, o.win, "confirm-dialog"), "nothing to ask");
    assert_eq!(
        o.shell.read_with(cx, |s, _| s.tab().unwrap().entity_id()),
        tab
    );
    assert_eq!(o.model(cx).read_with(cx, |m, _| m.edits().len()), 1);
}

#[gpui_kit::test]
fn a_second_swap_while_the_question_is_open_is_ignored(cx: &mut TestAppContext) {
    let o = opened(cx, T, "", 0);
    stage_cell(cx, &o);
    let (b, c) = (support::fixture("", ""), support::fixture("", ""));
    switch(cx, &o, b.dir.path().into());
    wait_real(cx, "the question", |cx| {
        present(cx, o.win, "confirm-dialog")
    });
    switch(cx, &o, c.dir.path().into());
    close_workspace(cx, &o);
    click_id(cx, o.win, "discard-close");
    assert!(!present(cx, o.win, "confirm-dialog"), "one question only");
    assert_eq!(current(cx, &o), Some(real(b.dir.path())));
}

#[gpui_kit::test]
fn back_to_back_swaps_land_on_the_last_and_drain_the_old_sessions(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let (b, c) = (support::fixture("", ""), support::fixture("", ""));
    switch(cx, &o, b.dir.path().into());
    switch(cx, &o, c.dir.path().into());
    assert_eq!(current(cx, &o), Some(real(c.dir.path())));
    assert!(present(cx, o.win, "connection-list"));
    wait_real(cx, "the old sessions to close", |cx| {
        o.shell.read_with(cx, |s, _| s.pending_closings() == 0)
    });
}

#[gpui_kit::test]
fn a_swap_or_a_close_while_the_window_closes_is_ignored(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let mut v = VisualTestContext::from_window(o.win, cx);
    assert!(
        !v.simulate_close(),
        "the open tab holds the window while it closes"
    );
    let other = support::fixture("", "");
    switch(cx, &o, other.dir.path().into());
    if cx.update(|cx| cx.windows().is_empty()) {
        return; // the window went before the swap could be asked for
    }
    close_workspace(cx, &o);
    assert_eq!(current(cx, &o), Some(real(o.fx.dir.path())));
    assert_eq!(
        saved(o.files.path()).last_workspace.as_deref(),
        Some(o.fx.dir.path())
    );
}

#[gpui_kit::test]
fn close_workspace_shows_the_welcome_view_and_the_next_start_does_too(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    command(cx, &o, "close workspace", "Close workspace");
    assert!(present(cx, o.win, "welcome-open"));
    assert!(!present(cx, o.win, "welcome-error"));
    assert!(o.shell.read_with(cx, |s, _| s.workspace().is_none()));
    assert!(!present(cx, o.win, "title-workspace"));
    let s = saved(o.files.path());
    assert_eq!(s.last_workspace, None);
    assert!(s.recent_workspaces.contains(&real(o.fx.dir.path())));
    wait_real(cx, "the old sessions to close", |cx| {
        o.shell.read_with(cx, |s, _| s.pending_closings() == 0)
    });
    let (win, _) = cx
        .update(|cx| start(support::services(o.files.path()), None, cx))
        .unwrap();
    assert!(present(cx, win, "welcome-open"));
}

#[gpui_kit::test]
fn closing_the_workspace_with_staged_edits_asks_and_keep_keeps_it(cx: &mut TestAppContext) {
    let o = opened(cx, T, "", 0);
    stage_cell(cx, &o);
    close_workspace(cx, &o);
    wait_real(cx, "the question", |cx| {
        present(cx, o.win, "confirm-dialog")
    });
    assert_eq!(
        label_of(cx, o.win, "confirm-detail").as_deref(),
        Some("Closing the workspace drops them.")
    );
    click_id(cx, o.win, "discard-keep");
    assert_eq!(current(cx, &o), Some(real(o.fx.dir.path())));
    assert_eq!(o.model(cx).read_with(cx, |m, _| m.edits().len()), 1);
    close_workspace(cx, &o);
    wait_real(cx, "the question", |cx| {
        present(cx, o.win, "confirm-dialog")
    });
    click_id(cx, o.win, "discard-close");
    assert!(present(cx, o.win, "welcome-open"));
}

#[gpui_kit::test]
fn the_title_pill_reports_its_menu_open_until_escape(cx: &mut TestAppContext) {
    let o = launched(cx, "", "");
    let state = |cx: &mut TestAppContext| expanded_of(cx, o.win, "title-workspace");
    assert_eq!(state(cx), Some(false), "closed before the first open");
    click_id(cx, o.win, "title-workspace");
    assert!(
        present(cx, o.win, "title-workspace-open"),
        "the menu opened"
    );
    assert_eq!(state(cx), Some(true), "open while the menu shows");
    cx.update_window(o.win, |_, w, cx| w.press("escape", cx))
        .unwrap();
    assert!(
        !present(cx, o.win, "title-workspace-open"),
        "Escape closed it"
    );
    assert_eq!(state(cx), Some(false), "closed after Escape");
}
