//! The explorer shows less at rest: the gear on hover, Reload in the menu, run timing once.
mod support;
use dbento_app::explorer::{connection_id, part_id};
use gpui_kit::TestAppContext;
use support::{TREE, click_id, label_of, opened, present, right_click_empty, started};

#[gpui_kit::test]
fn the_gear_shows_on_hover_or_selection(cx: &mut TestAppContext) {
    let s = started(cx, TREE);
    let gear = |name: &str| part_id("edit-connection", name);
    assert!(!present(cx, s.win, gear("top")));
    support::hover(cx, s.win, connection_id("top"));
    assert!(present(cx, s.win, gear("top")));
    support::hover(cx, s.win, connection_id("lite"));
    assert!(!present(cx, s.win, gear("top")));
    assert!(present(cx, s.win, gear("lite")));
    click_id(cx, s.win, connection_id("top"));
    support::hover(cx, s.win, connection_id("lite"));
    assert!(
        present(cx, s.win, gear("top")),
        "the selected row keeps its gear"
    );
}

#[gpui_kit::test]
fn reload_lives_in_the_empty_space_menu(cx: &mut TestAppContext) {
    let s = started(cx, TREE);
    assert!(!present(cx, s.win, "workspace-reload"));
    let more = format!("{TREE}\n[connections.aaa]\nengine = \"sqlite\"\npath = \"a.db\"\n");
    std::fs::write(s.dir.path().join("dbento.toml"), more).unwrap();
    right_click_empty(cx, &s);
    assert!(present(cx, s.win, "menu-reload"));
    click_id(cx, s.win, "menu-reload");
    assert!(present(cx, s.win, connection_id("aaa")));
}

#[gpui_kit::test]
fn timing_shows_only_in_the_result_footer(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    o.run(cx, "SELECT 1; SELECT 2");
    o.idle(cx);
    assert!(present(cx, o.win, "footer-elapsed"));
    assert!(!present(cx, o.win, "status-elapsed"));
    assert_eq!(
        label_of(cx, o.win, "run-summary").as_deref(),
        Some("2 statements")
    );
}
