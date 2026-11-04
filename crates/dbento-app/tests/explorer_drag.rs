mod support;
use dbento_app::explorer::{connection_id, drop_allowed, folder_id, part_id};
use dbento_core::{FolderPath, WorkspaceItem};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    AppContext as _, ElementId, InputEvent as _, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, TestAppContext, point, px,
};
use support::{Started, TREE, click_id, label_of, present, started, wait_real};

fn path(p: &str) -> FolderPath {
    p.parse().unwrap()
}
fn drag(cx: &mut TestAppContext, s: &Started, from: ElementId, to: ElementId) {
    cx.update_window(s.win, |_, w, cx| w.drag_to(from, to, cx))
        .unwrap();
    cx.run_until_parked();
}
fn drag_to_empty(cx: &mut TestAppContext, s: &Started, from: ElementId) {
    cx.update_window(s.win, |_, w, cx| {
        w.render_frame(cx);
        let a = w.find(from).bounds().center();
        let body = w.find("explorer-body").bounds();
        w.drag(
            a,
            point(body.origin.x + px(40.), body.bottom() - px(8.)),
            cx,
        );
    })
    .unwrap();
    cx.run_until_parked();
}

#[test]
fn drop_allowed_refuses_a_folder_over_itself_or_below() {
    let team = WorkspaceItem::Folder(path("team"));
    assert!(!drop_allowed(&team, Some(&path("team"))));
    assert!(!drop_allowed(&team, Some(&path("team/sub"))));
    assert!(drop_allowed(&team, Some(&path("teams"))));
    assert!(drop_allowed(&team, None));
    assert!(drop_allowed(
        &WorkspaceItem::Connection("lite".into()),
        Some(&path("team"))
    ));
}

#[gpui_kit::test]
fn a_connection_dropped_on_a_folder_moves_into_it_and_opens_no_tab(cx: &mut TestAppContext) {
    let s = started(cx, TREE);
    drag(cx, &s, connection_id("top"), folder_id(&path("team")));
    let text = s.toml();
    assert!(
        text.ends_with(
            "[connections.top]\nengine = \"sqlite\"\npath = \"t.db\"\nfolder = \"team\"\n"
        ),
        "{text}"
    );
    assert!(s.shell.read_with(cx, |sh, _| sh.tabs().is_empty()));
}

#[gpui_kit::test]
fn a_folder_dropped_on_empty_space_moves_to_the_top_level(cx: &mut TestAppContext) {
    let s = started(cx, TREE);
    drag_to_empty(cx, &s, folder_id(&path("team/sub")));
    assert!(s.toml().contains("folder = \"sub\"\n"), "{}", s.toml());
    assert!(present(cx, s.win, folder_id(&path("sub"))));
}

#[gpui_kit::test]
fn a_drop_on_a_connection_joins_its_folder(cx: &mut TestAppContext) {
    let s = started(cx, TREE);
    drag(cx, &s, connection_id("top"), connection_id("lite"));
    assert!(
        s.toml().ends_with("folder = \"team/sub\"\n"),
        "{}",
        s.toml()
    );
}

#[gpui_kit::test]
fn a_folder_dropped_on_a_table_row_joins_that_connections_folder(cx: &mut TestAppContext) {
    let s = started(cx, &format!("{TREE}\n[folders.ops]\n"));
    rusqlite::Connection::open(s.dir.path().join("l.db"))
        .unwrap()
        .execute_batch("CREATE TABLE Album(id INTEGER)")
        .unwrap();
    click_id(cx, s.win, part_id("connection-toggle", "lite"));
    wait_real(cx, "the schema row", |cx| {
        label_of(cx, s.win, ("tree-row", 0usize)).as_deref() == Some("lite.main")
    });
    let album = |cx: &mut TestAppContext| {
        (0..20usize)
            .find(|i| label_of(cx, s.win, ("tree-row", *i)).as_deref() == Some("lite.main.Album"))
    };
    wait_real(cx, "the table row", |cx| album(cx).is_some());
    let row = album(cx).unwrap();
    drag(
        cx,
        &s,
        folder_id(&path("ops")),
        ElementId::from(("tree-row", row)),
    );
    let text = s.toml();
    assert!(
        text.contains("[folders.\"team/sub/ops\"]") && !text.contains("[folders.ops]"),
        "{text}"
    );
}

#[gpui_kit::test]
fn a_folder_never_drops_into_itself(cx: &mut TestAppContext) {
    let s = started(cx, TREE);
    drag(
        cx,
        &s,
        folder_id(&path("team")),
        folder_id(&path("team/sub")),
    );
    assert_eq!(s.toml(), TREE);
    assert!(!present(cx, s.win, "save-error-ok"));
}

#[gpui_kit::test]
fn the_preview_shows_the_name_while_dragging(cx: &mut TestAppContext) {
    let s = started(cx, TREE);
    let named = cx
        .update_window(s.win, |_, w, cx| {
            w.render_frame(cx);
            let a = w.find(connection_id("top")).bounds().center();
            let to = point(a.x, a.y + px(30.));
            let down = MouseDownEvent {
                button: MouseButton::Left,
                position: a,
                modifiers: Default::default(),
                click_count: 1,
                first_mouse: false,
            };
            w.dispatch_event(down.to_platform_input(), cx);
            let moved = MouseMoveEvent {
                position: to,
                pressed_button: Some(MouseButton::Left),
                modifiers: Default::default(),
            };
            w.dispatch_event(moved.to_platform_input(), cx);
            w.render_frame(cx);
            let label = w
                .try_find("drag-preview")
                .and_then(|e| e.label().map(str::to_owned));
            let up = MouseUpEvent {
                button: MouseButton::Left,
                position: point(px(-50.), px(-50.)),
                modifiers: Default::default(),
                click_count: 1,
            };
            w.dispatch_event(up.to_platform_input(), cx);
            label
        })
        .unwrap();
    assert_eq!(named.as_deref(), Some("top"));
}

#[gpui_kit::test]
fn a_drop_into_the_current_folder_writes_nothing(cx: &mut TestAppContext) {
    let s = started(cx, TREE);
    let file = s.dir.path().join("dbento.toml");
    let before = std::fs::metadata(&file).unwrap().modified().unwrap();
    drag(cx, &s, connection_id("lite"), folder_id(&path("team/sub")));
    drag(
        cx,
        &s,
        folder_id(&path("team/sub")),
        folder_id(&path("team")),
    );
    assert_eq!(s.toml(), TREE);
    assert_eq!(
        std::fs::metadata(&file).unwrap().modified().unwrap(),
        before
    );
    assert!(!present(cx, s.win, "save-error-ok"));
}

#[test]
fn a_selected_row_keeps_its_accent_bar_under_a_drag() {
    use dbento_app::explorer::{DropBar, drop_bar};
    assert_eq!(drop_bar(true), DropBar::Accent);
    assert_eq!(drop_bar(false), DropBar::Drop);
}
