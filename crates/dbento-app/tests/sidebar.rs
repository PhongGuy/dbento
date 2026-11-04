mod support;
use dbento_app::app::start;
use dbento_app::explorer::{connection_id, folder_id, part_id};
use dbento_app::sidebar::{SidebarEvent, color_name, tag_color};
use dbento_core::Color;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext};
use std::cell::RefCell;
use std::rc::Rc;

#[gpui_kit::test]
fn every_connection_is_a_row_in_name_order_and_a_double_click_opens_it(cx: &mut TestAppContext) {
    support::init(cx);
    support::init_tokio(cx);
    let (fx, files) = (
        support::fixture("", "color = \"red\""),
        tempfile::tempdir().unwrap(),
    );
    let (win, shell) = cx
        .update(|cx| {
            start(
                support::services(files.path()),
                Some(fx.dir.path().into()),
                cx,
            )
        })
        .unwrap();
    assert_eq!(
        support::label_of(cx, win, connection_id("lite")).as_deref(),
        Some("lite")
    );
    assert_eq!(
        support::label_of(cx, win, part_id("connection-color", "lite")).as_deref(),
        Some("red")
    );
    assert!(!support::present(
        cx,
        win,
        part_id("connection-color", "nopg")
    ));
    assert_eq!(
        support::label_of(cx, win, connection_id("nopg")).as_deref(),
        Some("nopg")
    );
    // The rendered rows are exactly the workspace's connections: each named row is drawn and
    // a name the workspace lacks has none.
    for name in ["lite", "nopg"] {
        assert!(support::present(cx, win, connection_id(name)), "{name}");
    }
    assert!(!support::present(cx, win, connection_id("ghost")));
    let sidebar = shell.read_with(cx, |s, _| s.sidebar().cloned().unwrap());
    let opened = Rc::new(RefCell::new(Vec::new()));
    let seen = opened.clone();
    cx.update(|cx| {
        cx.subscribe(&sidebar, move |_, e: &SidebarEvent, _| {
            let SidebarEvent::Open(ix) = e else { return };
            seen.borrow_mut().push(*ix);
        })
        .detach()
    });
    support::open_connection_row(cx, win, "nopg");
    assert_eq!(*opened.borrow(), [1]);
    assert_eq!(sidebar.read_with(cx, |s, _| s.selected()), Some(1));
}
#[test]
fn every_tag_has_its_own_color_and_name() {
    let all = [
        Color::Red,
        Color::Orange,
        Color::Yellow,
        Color::Green,
        Color::Teal,
        Color::Blue,
        Color::Purple,
        Color::Gray,
    ];
    for (i, a) in all.iter().enumerate() {
        for b in &all[i + 1..] {
            assert_ne!(tag_color(*a), tag_color(*b), "{a:?} {b:?}");
        }
    }
    let names: Vec<&str> = all.iter().map(|c| color_name(*c)).collect();
    assert_eq!(
        names,
        [
            "red", "orange", "yellow", "green", "teal", "blue", "purple", "gray"
        ]
    );
}
#[gpui_kit::test]
fn folders_nest_their_connections_and_a_chevron_click_collapses_one(cx: &mut TestAppContext) {
    support::init(cx);
    let (fx, files) = (
        support::fixture(
            "",
            "folder = \"Prod/EU\"\n[folders.\"Prod\"]\ncolor = \"red\"",
        ),
        tempfile::tempdir().unwrap(),
    );
    let (win, _shell) = cx
        .update(|cx| {
            start(
                support::services(files.path()),
                Some(fx.dir.path().into()),
                cx,
            )
        })
        .unwrap();
    assert_eq!(
        support::label_of(cx, win, folder_id(&"Prod".parse().unwrap())).as_deref(),
        Some("Prod")
    );
    assert_eq!(
        support::label_of(cx, win, folder_id(&"Prod/EU".parse().unwrap())).as_deref(),
        Some("Prod/EU")
    );
    assert_eq!(
        support::label_of(cx, win, part_id("connection-color", "lite")).as_deref(),
        Some("red")
    );
    cx.update_window(win, |_, w, cx| {
        w.click(part_id("folder-toggle", "Prod"), cx)
    })
    .unwrap();
    cx.run_until_parked();
    assert!(!support::present(
        cx,
        win,
        folder_id(&"Prod/EU".parse().unwrap())
    ));
    assert!(!support::present(cx, win, connection_id("lite")));
    assert!(support::present(cx, win, connection_id("nopg")));
    cx.update_window(win, |_, w, cx| {
        w.click(part_id("folder-toggle", "Prod"), cx)
    })
    .unwrap();
    cx.run_until_parked();
    assert!(support::present(cx, win, connection_id("lite")));
}

#[gpui_kit::test]
fn no_catalog_session_starts_after_close_catalogs(cx: &mut gpui_kit::TestAppContext) {
    use dbento_app::catalog::CatalogQuery;
    let o = support::opened(cx, "", "", 0);
    let sidebar = o.shell.read_with(cx, |s, _| s.sidebar().cloned().unwrap());
    drop(sidebar.update(cx, |s, _| s.close_catalogs()));
    sidebar.update(cx, |s, cx| {
        s.ensure_loaded("lite", &[CatalogQuery::Schemas], cx)
    });
    assert_eq!(sidebar.read_with(cx, |s, _| s.catalog_sessions()), 0);
}
