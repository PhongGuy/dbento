mod support;
use dbento_app::app::{AppPaths, SavedState, WorkspaceSession, start};
use dbento_app::explorer::{connection_id, folder_id, part_id};
use dbento_app::persist::{load_json, save_json};
use dbento_core::{Edit, FolderPath, WorkspaceItem};
use gpui_kit::{AppContext as _, TestAppContext};
use support::{Started, TREE, click_id, label_of, present, started, wait_real};

fn path(p: &str) -> FolderPath {
    p.parse().unwrap()
}
fn apply(cx: &mut TestAppContext, s: &Started, edit: Edit) {
    let shell = s.shell.clone();
    cx.update_window(s.win, |_, w, cx| {
        shell.update(cx, |sh, cx| sh.apply_edit(edit, w, cx))
    })
    .unwrap();
    cx.run_until_parked();
}
fn saved_collapsed(cx: &mut TestAppContext, s: &Started) -> Vec<String> {
    let dir = s
        .shell
        .read_with(cx, |sh, _| sh.workspace().unwrap().dir().to_path_buf());
    let state = load_json::<SavedState>(&AppPaths::in_dir(s.files.path()).state).value;
    state
        .session(&dir)
        .map(|x| x.collapsed.clone())
        .unwrap_or_default()
}

#[gpui_kit::test]
fn a_folder_rename_is_written_installed_and_keeps_collapse(cx: &mut TestAppContext) {
    let s = started(cx, TREE);
    click_id(cx, s.win, part_id("folder-toggle", "team/sub"));
    apply(
        cx,
        &s,
        Edit::RenameFolder {
            from: path("team"),
            to: path("crew"),
        },
    );
    let text = s.toml();
    assert!(
        text.contains("[folders.crew]") && text.contains("folder = \"crew/sub\""),
        "{text}"
    );
    assert!(present(cx, s.win, folder_id(&path("crew"))));
    assert!(!present(cx, s.win, folder_id(&path("team"))));
    let sb = s.sidebar(cx);
    assert!(sb.read_with(cx, |b, _| b.is_collapsed(&path("crew/sub"))));
    assert!(
        !present(cx, s.win, connection_id("lite")),
        "still collapsed"
    );
    assert_eq!(saved_collapsed(cx, &s), ["crew/sub"]);
}

#[gpui_kit::test]
fn a_toggle_is_saved(cx: &mut TestAppContext) {
    let s = started(cx, TREE);
    click_id(cx, s.win, part_id("folder-toggle", "team"));
    assert_eq!(saved_collapsed(cx, &s), ["team"]);
    click_id(cx, s.win, part_id("folder-toggle", "team"));
    assert!(saved_collapsed(cx, &s).is_empty());
}

#[gpui_kit::test]
fn a_folder_click_selects_it_and_only_its_chevron_collapses_it(cx: &mut TestAppContext) {
    let s = started(cx, TREE);
    let sb = s.sidebar(cx);
    sb.update(cx, |b, cx| b.select(0, cx));
    click_id(cx, s.win, folder_id(&path("team")));
    // The folder took the selection from the connection and stayed open.
    assert_eq!(
        sb.read_with(cx, |b, _| (b.selected(), b.selected_node())),
        (None, None)
    );
    assert!(present(cx, s.win, folder_id(&path("team/sub"))));
    assert!(present(cx, s.win, connection_id("lite")));
    click_id(cx, s.win, part_id("folder-toggle", "team"));
    assert!(sb.read_with(cx, |b, _| b.is_collapsed(&path("team"))));
    assert!(!present(cx, s.win, connection_id("lite")));
}

#[gpui_kit::test]
fn saved_collapse_is_restored_on_start(cx: &mut TestAppContext) {
    support::init(cx);
    support::init_tokio(cx);
    let (dir, files) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    std::fs::write(dir.path().join("dbento.toml"), TREE).unwrap();
    let mut state = SavedState::default();
    state.put_session(WorkspaceSession {
        workspace: std::fs::canonicalize(dir.path()).unwrap(),
        collapsed: vec!["team/sub".into(), "gone".into()],
        ..Default::default()
    });
    save_json(&AppPaths::in_dir(files.path()).state, &state).unwrap();
    let (win, shell) = cx
        .update(|cx| start(support::services(files.path()), Some(dir.path().into()), cx))
        .unwrap();
    present(cx, win, "connection-list");
    let sb = shell.read_with(cx, |s, _| s.sidebar().cloned().unwrap());
    assert!(sb.read_with(cx, |b, _| b.is_collapsed(&path("team/sub"))));
    assert!(!sb.read_with(cx, |b, _| b.is_collapsed(&path("gone"))));
    assert!(present(cx, win, folder_id(&path("team/sub"))));
    assert!(!present(cx, win, connection_id("lite")));
}

#[gpui_kit::test]
fn a_changed_file_offers_reload_and_writes_nothing(cx: &mut TestAppContext) {
    let s = started(cx, TREE);
    let edited = format!("{TREE}# by hand\n");
    std::fs::write(s.dir.path().join("dbento.toml"), &edited).unwrap();
    apply(
        cx,
        &s,
        Edit::PutFolder {
            path: path("new"),
            color: None,
        },
    );
    wait_real(cx, "the reload offer", |cx| {
        present(cx, s.win, "edit-reload")
    });
    assert_eq!(s.toml(), edited);
    click_id(cx, s.win, "edit-reload");
    let text = s
        .shell
        .read_with(cx, |sh, _| sh.workspace().unwrap().source_text().to_owned());
    assert_eq!(text, edited);
}

#[gpui_kit::test]
fn a_refused_edit_says_why_and_writes_nothing(cx: &mut TestAppContext) {
    let s = started(cx, TREE);
    apply(
        cx,
        &s,
        Edit::RenameFolder {
            from: path("team/sub"),
            to: path("team"),
        },
    );
    wait_real(cx, "the error", |cx| present(cx, s.win, "save-error-ok"));
    let says = label_of(cx, s.win, "confirm-detail").unwrap();
    assert!(says.contains("already exists"), "{says}");
    assert_eq!(s.toml(), TREE);
}

#[gpui_kit::test]
fn an_edit_that_changes_nothing_writes_nothing(cx: &mut TestAppContext) {
    let s = started(cx, TREE);
    // A symlinked file refuses every write, so only a skipped write passes silently.
    let real = s.dir.path().join("real.toml");
    std::fs::write(&real, TREE).unwrap();
    let file = s.dir.path().join("dbento.toml");
    std::fs::remove_file(&file).unwrap();
    std::os::unix::fs::symlink(&real, &file).unwrap();
    let top = || WorkspaceItem::Connection("top".into());
    apply(
        cx,
        &s,
        Edit::Move {
            item: top(),
            to: None,
        },
    );
    assert!(!present(cx, s.win, "save-error-ok"));
    apply(
        cx,
        &s,
        Edit::Move {
            item: top(),
            to: Some(path("team")),
        },
    );
    wait_real(cx, "the symlink refusal", |cx| {
        present(cx, s.win, "save-error-ok")
    });
}
