//! The app state file: written off the UI turn, once per burst, and read only at start.
use crate::support;
use crate::support::{Started, TREE, present, started};
use dbento_app::app::WorkspaceSession;
use dbento_app::app::{AppPaths, SavedState, start};
use dbento_app::explorer::part_id;
use dbento_app::persist::{load_json, save_json};
use dbento_core::seen_text;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext};

fn state(s: &Started) -> std::path::PathBuf {
    AppPaths::in_dir(s.files.path()).state
}
fn collapsed(s: &Started) -> Vec<String> {
    let saved = load_json::<SavedState>(&state(s)).value;
    let dir = std::fs::canonicalize(s.dir.path()).unwrap();
    saved
        .session(&dir)
        .map(|x| x.collapsed.clone())
        .unwrap_or_default()
}
fn toggle_team(s: &Started, times: usize, cx: &mut TestAppContext) {
    cx.update_window(s.win, |_, w, cx| {
        for _ in 0..times {
            w.click(part_id("folder-toggle", "team"), cx);
        }
    })
    .unwrap();
}

#[gpui_kit::test]
fn a_burst_of_collapse_toggles_writes_the_state_once_after_the_turn(cx: &mut TestAppContext) {
    let s = started(cx, TREE);
    cx.run_until_parked();
    let (writes, before) = (
        s.shell.read_with(cx, |sh, _| sh.state_writes()),
        std::fs::read_to_string(state(&s)).unwrap(),
    );
    toggle_team(&s, 3, cx);
    assert_eq!(
        std::fs::read_to_string(state(&s)).unwrap(),
        before,
        "the UI turn itself writes nothing"
    );
    cx.run_until_parked();
    assert_eq!(collapsed(&s), ["team"]);
    assert_eq!(s.shell.read_with(cx, |sh, _| sh.state_writes()), writes + 1);
}

#[gpui_kit::test]
fn a_change_still_unwritten_lands_when_the_window_closes(cx: &mut TestAppContext) {
    let s = started(cx, TREE);
    cx.run_until_parked();
    toggle_team(&s, 1, cx);
    let shell = s.shell.clone();
    let closes = cx
        .update_window(s.win, |_, w, cx| {
            shell.update(cx, |sh, cx| sh.request_close(w, cx))
        })
        .unwrap();
    assert!(closes);
    assert_eq!(collapsed(&s), ["team"]);
}

#[gpui_kit::test]
fn opening_a_workspace_reads_the_state_file_only_in_its_one_background_write(
    cx: &mut TestAppContext,
) {
    support::init(cx);
    support::init_tokio(cx);
    let (dir, files) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    std::fs::write(dir.path().join("dbento.toml"), TREE).unwrap();
    let (win, shell) = cx
        .update(|cx| start(support::services(files.path()), None, cx))
        .unwrap();
    cx.run_until_parked();
    // A read of this file sets it aside as state.json.bad; only the background writer may.
    let path = AppPaths::in_dir(files.path()).state;
    std::fs::write(&path, "not json").unwrap();
    cx.update_window(win, |_, w, cx| {
        shell.update(cx, |sh, cx| sh.open_workspace(dir.path().into(), w, cx))
    })
    .unwrap();
    assert!(!path.with_file_name("state.json.bad").exists());
    assert!(present(cx, win, "connection-list"));
    cx.run_until_parked();
    let saved = load_json::<SavedState>(&path);
    assert_eq!(saved.recovered, None);
    assert_eq!(saved.value.last_workspace.as_deref(), Some(dir.path()));
    assert_eq!(shell.read_with(cx, |sh, _| sh.state_writes()), 1);
}

#[gpui_kit::test]
fn another_instance_s_change_survives_this_shell_s_write(cx: &mut TestAppContext) {
    let s = started(cx, TREE);
    cx.run_until_parked();
    // Instance B writes a field this shell never changes, between this shell's writes.
    let mut other = load_json::<SavedState>(&state(&s)).value;
    other.sidebar_width = Some(321.);
    save_json(&state(&s), &other).unwrap();
    toggle_team(&s, 1, cx);
    cx.run_until_parked();
    assert_eq!(collapsed(&s), ["team"]);
    let saved = load_json::<SavedState>(&state(&s)).value;
    assert_eq!(saved.sidebar_width, Some(321.));
    // The synchronous write at close keeps it too.
    other = saved;
    other.editor_height = Some(123.);
    save_json(&state(&s), &other).unwrap();
    toggle_team(&s, 1, cx);
    let shell = s.shell.clone();
    cx.update_window(s.win, |_, w, cx| {
        shell.update(cx, |sh, cx| sh.request_close(w, cx))
    })
    .unwrap();
    let saved = load_json::<SavedState>(&state(&s)).value;
    assert!(collapsed(&s).is_empty());
    assert_eq!(saved.editor_height, Some(123.));
}

fn seen(s: &Started) -> Option<String> {
    let dir = std::fs::canonicalize(s.dir.path()).unwrap();
    load_json::<SavedState>(&state(s))
        .value
        .session(&dir)
        .and_then(|x| x.seen.clone())
}

#[test]
fn a_session_without_a_snapshot_writes_no_seen_key_and_one_with_it_round_trips() {
    let bare = WorkspaceSession {
        workspace: "/w".into(),
        ..Default::default()
    };
    assert!(!serde_json::to_string(&bare).unwrap().contains("seen"));
    let kept = WorkspaceSession {
        seen: Some("dbento = 1\n".into()),
        ..bare
    };
    let back: WorkspaceSession =
        serde_json::from_str(&serde_json::to_string(&kept).unwrap()).unwrap();
    assert_eq!(back, kept);
}

#[test]
fn an_older_state_file_without_snapshots_loads() {
    let old = r#"{"last_workspace":"/w","sessions":[{"workspace":"/w","tabs":[],"active":null,"collapsed":["team"]}]}"#;
    let saved: SavedState = serde_json::from_str(old).unwrap();
    let s = saved.session(std::path::Path::new("/w")).unwrap();
    assert_eq!(s.seen, None);
    assert_eq!(s.collapsed, ["team"]);
}

#[gpui_kit::test]
fn the_session_keeps_the_installed_connection_list_without_comments(cx: &mut TestAppContext) {
    let s = started(cx, &format!("{TREE}# old password Hk4pQ9zr\n"));
    // state.json is written on a background task, so every read waits for the write to land.
    support::wait_real(cx, "the first snapshot", |_| seen(&s).is_some());
    let ws = s
        .shell
        .read_with(cx, |sh, _| sh.workspace().cloned().unwrap());
    let first = seen(&s).unwrap();
    assert_eq!(Some(first.clone()), seen_text(&ws));
    assert!(
        !first.contains("Hk4pQ9zr") && first.contains("[connections.top]"),
        "{first}"
    );
    // An outside edit installed through the reload replaces the snapshot.
    std::fs::write(
        s.dir.path().join("dbento.toml"),
        TREE.replace("[connections.top]", "[connections.top2]"),
    )
    .unwrap();
    let shell = s.shell.clone();
    cx.update_window(s.win, |_, w, cx| {
        shell.update(cx, |sh, cx| sh.workspace_files_changed(w, cx))
    })
    .unwrap();
    support::settle_reload(cx, &s.shell);
    support::wait_real(cx, "the reloaded snapshot", |_| {
        seen(&s).is_some_and(|x| x.contains("[connections.top2]"))
    });
    let after = seen(&s).unwrap();
    assert!(
        after.contains("[connections.top2]") && !after.contains("[connections.top]"),
        "{after}"
    );
}
