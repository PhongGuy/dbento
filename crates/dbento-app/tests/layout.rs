use crate::support;
use crate::support::{label_of, opened, present, wait_real};
use dbento_app::app::{AppPaths, MAX_RECENT, SavedState, WindowState, start, window_options_for};
use dbento_app::persist::{load_json, save_json};
use dbento_core::{RelationKind, RelationRef};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext, VisualTestContext, WindowBounds, px, size};
use std::path::{Path, PathBuf};

fn saved(files: &Path) -> SavedState {
    load_json(&AppPaths::in_dir(files).state).value
}
fn closed(cx: &mut TestAppContext) -> bool {
    cx.update(|cx| cx.windows().is_empty())
}

#[gpui_kit::test]
fn the_window_opens_at_its_saved_size(cx: &mut TestAppContext) {
    support::init(cx);
    let files = tempfile::tempdir().unwrap();
    let state = SavedState {
        window: Some(WindowState {
            x: 10.,
            y: 20.,
            width: 900.,
            height: 700.,
            maximized: false,
        }),
        ..SavedState::default()
    };
    save_json(&AppPaths::in_dir(files.path()).state, &state).unwrap();
    let (win, _) = cx
        .update(|cx| start(support::services(files.path()), None, cx))
        .unwrap();
    let got = cx.update_window(win, |_, w, _| w.bounds().size).unwrap();
    assert_eq!(got, size(px(900.), px(700.)));
}

#[gpui_kit::test]
fn closing_saves_the_window_size(cx: &mut TestAppContext) {
    support::init(cx);
    let files = tempfile::tempdir().unwrap();
    let (win, _) = cx
        .update(|cx| start(support::services(files.path()), None, cx))
        .unwrap();
    cx.simulate_window_resize(win, size(px(1000.), px(650.)));
    cx.run_until_parked();
    assert!(VisualTestContext::from_window(win, cx).simulate_close());
    let w = saved(files.path()).window.unwrap();
    assert_eq!((w.width, w.height, w.maximized), (1000., 650., false));
}

#[test]
fn saved_sizes_are_clamped() {
    let tiny = WindowState {
        x: 0.,
        y: 0.,
        width: 10.,
        height: 10.,
        maximized: false,
    };
    let Some(WindowBounds::Windowed(b)) = window_options_for(Some(&tiny)).window_bounds else {
        panic!()
    };
    assert_eq!(b.size, size(px(480.), px(320.)));
    let huge = WindowState {
        width: 1e9,
        height: 1e9,
        maximized: true,
        ..tiny
    };
    let Some(WindowBounds::Maximized(b)) = window_options_for(Some(&huge)).window_bounds else {
        panic!()
    };
    assert_eq!(b.size, size(px(16384.), px(16384.)));
    assert!(window_options_for(None).window_bounds.is_none());
}

#[gpui_kit::test]
fn the_sidebar_width_and_editor_height_are_saved_and_restored(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let shell = o.shell.clone();
    let split = shell.read_with(cx, |s, _| s.split_state().clone());
    let tab_split = shell.read_with(cx, |s, cx| s.tab().unwrap().read(cx).split_state().clone());
    cx.update_window(o.win, |_, w, cx| {
        split.update(cx, |s, cx| s.resize_panel(0, px(300.), w, cx));
        tab_split.update(cx, |s, cx| s.resize_panel(0, px(200.), w, cx));
    })
    .unwrap();
    cx.run_until_parked();
    let mut v = VisualTestContext::from_window(o.win, cx);
    assert!(!v.simulate_close());
    wait_real(cx, "the window to close", closed);
    let s = saved(o.files.path());
    assert!((s.sidebar_width.unwrap() - 300.).abs() < 1., "{s:?}");
    assert!((s.editor_height.unwrap() - 200.).abs() < 1., "{s:?}");
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
    let width = cx
        .update_window(win, |_, w, cx| {
            w.render_frame(cx);
            w.find("sidebar").bounds().size.width
        })
        .unwrap();
    assert!((f32::from(width) - 300.).abs() < 1., "{width:?}");
    let height = shell.read_with(cx, |s, cx| s.tab().unwrap().read(cx).editor_height(cx));
    assert!((height.unwrap() - 200.).abs() < 1., "{height:?}");
}

#[test]
fn recent_workspaces_are_newest_first_unique_and_capped() {
    let mut s = SavedState::default();
    for i in 0..12 {
        s.remember_workspace(Path::new(&format!("/w{i}")));
    }
    s.remember_workspace(Path::new("/w5"));
    assert_eq!(s.recent_workspaces.len(), MAX_RECENT);
    assert_eq!(s.recent_workspaces[0], PathBuf::from("/w5"));
    assert_eq!(
        s.recent_workspaces
            .iter()
            .filter(|p| p.as_path() == Path::new("/w5"))
            .count(),
        1
    );
    s.forget_workspace(Path::new("/w5"));
    assert_eq!(s.recent_workspaces[0], PathBuf::from("/w11"));
}

#[gpui_kit::test]
fn the_welcome_view_lists_recent_workspaces_and_opens_one(cx: &mut TestAppContext) {
    support::init(cx);
    let (fx, files) = (support::fixture("", ""), tempfile::tempdir().unwrap());
    let state = SavedState {
        recent_workspaces: vec![fx.dir.path().into()],
        ..SavedState::default()
    };
    save_json(&AppPaths::in_dir(files.path()).state, &state).unwrap();
    let (win, _) = cx
        .update(|cx| start(support::services(files.path()), None, cx))
        .unwrap();
    let label = label_of(cx, win, ("recent-workspace", 0usize));
    assert_eq!(label.as_deref(), Some(fx.dir.path().to_str().unwrap()));
    cx.update_window(win, |_, w, cx| w.click(("recent-workspace", 0usize), cx))
        .unwrap();
    assert!(present(cx, win, "connection-list"));
    assert_eq!(
        saved(files.path()).recent_workspaces,
        [fx.dir.path().to_path_buf()]
    );
}

#[gpui_kit::test]
fn an_open_history_panel_comes_back_open(cx: &mut TestAppContext) {
    support::init(cx);
    let (fx, files) = (support::fixture("", ""), tempfile::tempdir().unwrap());
    let state = SavedState {
        history_open: true,
        ..SavedState::default()
    };
    save_json(&AppPaths::in_dir(files.path()).state, &state).unwrap();
    let (win, _) = cx
        .update(|cx| {
            start(
                support::services(files.path()),
                Some(fx.dir.path().into()),
                cx,
            )
        })
        .unwrap();
    assert!(present(cx, win, "history-panel"));
}

#[gpui_kit::test]
fn a_narrow_window_keeps_the_side_panels_inside_it(cx: &mut TestAppContext) {
    support::init(cx);
    let (fx, files) = (support::fixture("", ""), tempfile::tempdir().unwrap());
    let state = SavedState {
        window: Some(WindowState {
            x: 0.,
            y: 0.,
            width: 480.,
            height: 320.,
            maximized: false,
        }),
        history_open: true,
        ..SavedState::default()
    };
    save_json(&AppPaths::in_dir(files.path()).state, &state).unwrap();
    let (win, shell) = cx
        .update(|cx| {
            start(
                support::services(files.path()),
                Some(fx.dir.path().into()),
                cx,
            )
        })
        .unwrap();
    let check = |cx: &mut TestAppContext| {
        assert!(present(cx, win, "history-panel"));
        let (sidebar, tabs, dock) = cx
            .update_window(win, |_, w, cx| {
                w.render_frame(cx);
                (
                    w.find("sidebar").bounds(),
                    w.find("tab-area").bounds(),
                    w.find("history-panel").bounds(),
                )
            })
            .unwrap();
        assert!(
            f32::from(dock.right()) <= 480.5,
            "{sidebar:?} {tabs:?} {dock:?}"
        );
        assert!(f32::from(sidebar.size.width) >= 99., "{sidebar:?}");
        assert!(f32::from(tabs.size.width) >= 99., "{tabs:?}");
    };
    check(cx);
    // Opening the dock in an already narrow window must not push it out either.
    shell.update(cx, |s, cx| s.toggle_history(cx));
    cx.run_until_parked();
    shell.update(cx, |s, cx| s.toggle_history(cx));
    check(cx);
}

#[cfg(unix)]
#[gpui_kit::test]
fn a_workspace_opened_through_a_symlink_is_remembered_once_by_its_real_path(
    cx: &mut TestAppContext,
) {
    support::init(cx);
    let (fx, files) = (support::fixture("", ""), tempfile::tempdir().unwrap());
    let link = files.path().join("link");
    std::os::unix::fs::symlink(fx.dir.path(), &link).unwrap();
    let real = std::fs::canonicalize(fx.dir.path()).unwrap();
    let (win, shell) = cx
        .update(|cx| start(support::services(files.path()), Some(link), cx))
        .unwrap();
    assert!(present(cx, win, "connection-list"));
    cx.update_window(win, |_, w, cx| {
        shell.update(cx, |s, cx| s.open_workspace(real.clone(), w, cx))
    })
    .unwrap();
    assert_eq!(saved(files.path()).recent_workspaces, [real]);
}

#[gpui_kit::test]
fn a_saved_editor_height_taller_than_the_window_leaves_room_for_results(cx: &mut TestAppContext) {
    support::init(cx);
    support::init_tokio(cx);
    let (fx, files) = (support::fixture("", ""), tempfile::tempdir().unwrap());
    let state = SavedState {
        window: Some(WindowState {
            x: 0.,
            y: 0.,
            width: 900.,
            height: 600.,
            maximized: false,
        }),
        editor_height: Some(5000.),
        ..SavedState::default()
    };
    save_json(&AppPaths::in_dir(files.path()).state, &state).unwrap();
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
    support::open_connection_row(cx, win, "lite");
    let split = shell.read_with(cx, |s, cx| s.tab().unwrap().read(cx).split_state().clone());
    for _ in 0..3 {
        cx.update_window(win, |_, w, cx| w.render_frame(cx))
            .unwrap();
        cx.run_until_parked();
    }
    let sizes = split.read_with(cx, |s, _| s.sizes().clone());
    assert!(f32::from(sizes[1]) >= 150., "{sizes:?}");
}

#[test]
fn a_nan_window_bound_loses_only_the_window() {
    let files = tempfile::tempdir().unwrap();
    let path = AppPaths::in_dir(files.path()).state;
    let state = SavedState {
        window: Some(WindowState {
            x: f32::NAN,
            y: 0.,
            width: 900.,
            height: 600.,
            maximized: false,
        }),
        recent_workspaces: vec![PathBuf::from("/w")],
        history_open: true,
        ..SavedState::default()
    };
    save_json(&path, &state).unwrap();
    let loaded = load_json::<SavedState>(&path).value;
    assert_eq!(loaded.recent_workspaces, [PathBuf::from("/w")]);
    assert!(loaded.history_open);
    assert_eq!(loaded.window, None);
}

#[gpui_kit::test]
fn a_table_tab_does_not_become_the_saved_editor_height(cx: &mut TestAppContext) {
    let o = opened(
        cx,
        "CREATE TABLE Album(AlbumId INTEGER PRIMARY KEY);",
        "",
        0,
    );
    let shell = o.shell.clone();
    cx.update_window(o.win, |_, w, cx| {
        shell.update(cx, |s, cx| {
            s.open_relation(
                0,
                &RelationRef::new("main", "Album"),
                RelationKind::Table,
                w,
                cx,
            )
        })
    })
    .unwrap();
    cx.run_until_parked();
    // A laid-out split, so the table tab has an editor height to (wrongly) save.
    assert!(present(cx, o.win, "editor"));
    let mut v = VisualTestContext::from_window(o.win, cx);
    assert!(!v.simulate_close());
    wait_real(cx, "the window to close", closed);
    assert_eq!(saved(o.files.path()).editor_height, None);
}
