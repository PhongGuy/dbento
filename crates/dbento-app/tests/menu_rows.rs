//! Menu rows with key chips, and the sidebar's new-query and notice events.
mod support;
use dbento_app::keymap::{NEW_CONNECTION_KEYS, chip_keys};
use dbento_app::sidebar::SidebarEvent;
use dbento_app::ui::{Notice, NoticeKind};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext, px};
use support::{TREE, label_of, present, right_click_empty, started};

#[gpui_kit::test]
fn a_menu_row_shows_its_keys_and_only_then(cx: &mut TestAppContext) {
    let s = started(cx, TREE);
    right_click_empty(cx, &s);
    assert_eq!(
        label_of(cx, s.win, "menu-new-connection-keys"),
        Some(chip_keys(NEW_CONNECTION_KEYS))
    );
    assert!(present(cx, s.win, "menu-new-folder"));
    assert!(!present(cx, s.win, "menu-new-folder-keys"));
}

#[gpui_kit::test]
fn new_query_opens_a_tab_holding_its_sql_unrun(cx: &mut TestAppContext) {
    let s = started(cx, TREE);
    let sidebar = s.sidebar(cx);
    sidebar.update(cx, |_, cx| {
        cx.emit(SidebarEvent::NewQuery {
            connection: "top".into(),
            sql: "SELECT 1".into(),
        })
    });
    cx.run_until_parked();
    let (conn, sql, ran) = s.shell.read_with(cx, |sh, cx| {
        let t = sh.tab().expect("a new tab").read(cx);
        let m = t.model().read(cx);
        (
            m.spec().connection.clone(),
            t.sql(cx),
            !m.results().is_empty(),
        )
    });
    assert_eq!(
        (conn.as_str(), sql.as_str(), ran),
        ("top", "SELECT 1", false)
    );
}

#[gpui_kit::test]
fn a_sidebar_notice_reaches_the_toast_layer(cx: &mut TestAppContext) {
    let s = started(cx, TREE);
    let sidebar = s.sidebar(cx);
    sidebar.update(cx, |_, cx| {
        cx.emit(SidebarEvent::Notice(Notice {
            text: "Counted".into(),
            kind: NoticeKind::Info,
        }))
    });
    assert_eq!(label_of(cx, s.win, "notice").as_deref(), Some("Counted"));
}

// The kit forces its own menu icons to 12 px, so the row draws its glyph itself.
#[gpui_kit::test]
fn a_menu_glyph_is_drawn_at_14_px(cx: &mut TestAppContext) {
    let s = started(cx, TREE);
    right_click_empty(cx, &s);
    let size = cx
        .update_window(s.win, |_, w, cx| {
            w.render_frame(cx);
            w.find("menu-new-connection-icon").bounds().size
        })
        .unwrap();
    assert_eq!((size.width, size.height), (px(14.), px(14.)));
}
