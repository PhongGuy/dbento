mod support;
use dbento_app::explorer::{connection_id, folder_id, part_id, shown_depth};
use dbento_core::FolderPath;
use gpui_kit::TestAppContext;
use support::{TREE, click_id, label_of, present, reload, started};

fn path(p: &str) -> FolderPath {
    p.parse().unwrap()
}

#[gpui_kit::test]
fn rows_are_found_by_name_and_path(cx: &mut TestAppContext) {
    let s = started(cx, TREE);
    assert_eq!(
        label_of(cx, s.win, folder_id(&path("team"))).as_deref(),
        Some("team")
    );
    assert_eq!(
        label_of(cx, s.win, folder_id(&path("team/sub"))).as_deref(),
        Some("team/sub")
    );
    assert_eq!(
        label_of(cx, s.win, connection_id("lite")).as_deref(),
        Some("lite")
    );
    assert_eq!(
        label_of(cx, s.win, part_id("connection-color", "lite")).as_deref(),
        Some("blue")
    );
    assert!(!present(cx, s.win, part_id("connection-color", "top")));
    support::hover(cx, s.win, connection_id("top"));
    assert!(present(cx, s.win, part_id("edit-connection", "top")));
    assert!(present(cx, s.win, part_id("edit-glyph", "top")));
    assert!(present(cx, s.win, part_id("connection-toggle", "top")));
}

#[gpui_kit::test]
fn an_id_follows_its_row_when_rows_shift(cx: &mut TestAppContext) {
    let s = started(cx, TREE);
    let more = format!("{TREE}\n[connections.aaa]\nengine = \"sqlite\"\npath = \"a.db\"\n");
    std::fs::write(s.dir.path().join("dbento.toml"), more).unwrap();
    reload(cx, s.win);
    assert_eq!(
        label_of(cx, s.win, connection_id("top")).as_deref(),
        Some("top")
    );
    assert_eq!(
        label_of(cx, s.win, connection_id("aaa")).as_deref(),
        Some("aaa")
    );
    click_id(cx, s.win, part_id("folder-toggle", "team"));
    assert!(!present(cx, s.win, connection_id("lite")));
    assert!(present(cx, s.win, connection_id("top")));
}

#[test]
fn explorer_depth_stops_growing_past_eight_levels() {
    assert_eq!(shown_depth(0), 0);
    assert_eq!(shown_depth(8), 8);
    assert_eq!(shown_depth(30), 8);
}

#[gpui_kit::test]
fn a_connection_that_goes_and_comes_back_starts_idle(cx: &mut TestAppContext) {
    use dbento_app::link::LinkState;
    let s = started(cx, TREE);
    let sidebar = s
        .shell
        .read_with(cx, |sh, _| sh.sidebar().cloned().unwrap());
    let links = std::collections::HashMap::from([("top".to_owned(), LinkState::Connected)]);
    sidebar.update(cx, |sb, cx| sb.set_session_links(links, cx));
    let gone = TREE.split("\n[connections.top]").next().unwrap();
    std::fs::write(s.dir.path().join("dbento.toml"), gone).unwrap();
    reload(cx, s.win);
    std::fs::write(s.dir.path().join("dbento.toml"), TREE).unwrap();
    reload(cx, s.win);
    assert_eq!(
        sidebar.read_with(cx, |sb, _| sb.link("top")),
        LinkState::Idle
    );
}

// The foot reads the sidebar's link, so the shell must redraw when the sidebar changes.
#[gpui_kit::test]
fn the_shell_redraws_when_the_explorer_changes(cx: &mut TestAppContext) {
    let s = started(cx, TREE);
    let sidebar = s
        .shell
        .read_with(cx, |sh, _| sh.sidebar().cloned().unwrap());
    let seen = std::rc::Rc::new(std::cell::Cell::new(0));
    let count = seen.clone();
    let _watch = cx.update(|cx| cx.observe(&s.shell, move |_, _| count.set(count.get() + 1)));
    sidebar.update(cx, |_, cx| cx.notify());
    cx.run_until_parked();
    assert!(seen.get() > 0, "the shell never heard of the change");
}
