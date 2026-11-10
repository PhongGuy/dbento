use crate::support;
use dbento_app::explorer::{connection_id, part_id};
use std::sync::Arc;

use crate::support::{label_of, opened, present, reload, rewrite, wait_real};
use dbento_app::schema_tree::NodeKey;
use dbento_core::FolderPath;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext};

const AAA: &str = "[connections.aaa]\nengine = \"sqlite\"\npath = \"data/app.db\"\n";

fn sidebar(
    cx: &mut TestAppContext,
    o: &support::Opened,
) -> gpui_kit::Entity<dbento_app::sidebar::Sidebar> {
    o.shell.read_with(cx, |s, _| s.sidebar().cloned().unwrap())
}

#[gpui_kit::test]
fn reload_keeps_selection_collapse_and_expansion_by_name(cx: &mut TestAppContext) {
    let o = opened(cx, "", "folder = \"team\"", 0);
    let sb = sidebar(cx, &o);
    sb.update(cx, |s, cx| {
        s.toggle_node(NodeKey::Connection("lite".into()), cx)
    });
    cx.update_window(o.win, |_, w, cx| {
        w.click(part_id("folder-toggle", "team"), cx)
    })
    .unwrap();
    cx.run_until_parked();
    let team: FolderPath = "team".parse().unwrap();
    assert!(sb.read_with(cx, |s, _| s.is_collapsed(&team)));
    assert_eq!(sb.read_with(cx, |s, _| s.selected()), Some(0));

    rewrite(&o.fx, "folder = \"team\"", AAA);
    reload(cx, o.win);

    let same = o.shell.read_with(cx, |s, _| s.sidebar().cloned().unwrap());
    assert_eq!(
        same.entity_id(),
        sb.entity_id(),
        "the sidebar keeps its entity"
    );
    assert_eq!(
        sb.read_with(cx, |s, _| s.selected()),
        Some(1),
        "lite moved to index 1 behind aaa"
    );
    assert!(sb.read_with(cx, |s, _| s.is_collapsed(&team)));
    assert!(sb.read_with(cx, |s, _| {
        s.tree().is_expanded(&NodeKey::Connection("lite".into()))
    }));
    assert_eq!(
        label_of(cx, o.win, connection_id("aaa")).as_deref(),
        Some("aaa")
    );
    assert!(o.shell.read_with(cx, |s, _| {
        s.workspace().unwrap().connection("aaa").is_some()
    }));
}

#[gpui_kit::test]
fn a_reload_closes_only_changed_catalog_sessions(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let sb = sidebar(cx, &o);
    sb.update(cx, |s, cx| {
        s.toggle_node(NodeKey::Connection("lite".into()), cx)
    });
    wait_real(cx, "the lite catalog", |cx| {
        sb.read_with(cx, |s, _| s.catalog_sessions() == 1)
    });
    assert_eq!(sb.read_with(cx, |s, _| s.catalogs_started()), 1);

    rewrite(&o.fx, "color = \"green\"", "");
    reload(cx, o.win);
    assert_eq!(
        sb.read_with(cx, |s, _| (s.catalogs_started(), s.catalog_sessions())),
        (1, 1),
        "a colour is not a session change"
    );

    rewrite(&o.fx, "read_only = true", "");
    reload(cx, o.win);
    wait_real(cx, "a new lite catalog", |cx| {
        sb.read_with(cx, |s, _| s.catalogs_started() == 2)
    });
    assert_eq!(sb.read_with(cx, |s, _| s.catalog_sessions()), 1);
    assert!(sb.read_with(cx, |s, _| {
        s.tree().is_expanded(&NodeKey::Connection("lite".into()))
    }));
}

#[gpui_kit::test]
fn a_gone_connection_loses_its_catalog_and_expansion(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let sb = sidebar(cx, &o);
    sb.update(cx, |s, cx| {
        s.toggle_node(NodeKey::Connection("lite".into()), cx)
    });
    wait_real(cx, "the lite catalog", |cx| {
        sb.read_with(cx, |s, _| s.catalog_sessions() == 1)
    });
    std::fs::write(
        o.fx.dir.path().join("dbento.toml"),
        format!("dbento = 1\n{AAA}"),
    )
    .unwrap();
    reload(cx, o.win);
    assert_eq!(
        sb.read_with(cx, |s, _| (s.catalog_sessions(), s.catalogs_started())),
        (0, 1)
    );
    assert!(!sb.read_with(cx, |s, _| {
        s.tree().is_expanded(&NodeKey::Connection("lite".into()))
    }));
    assert_eq!(sb.read_with(cx, |s, _| s.selected()), None);
    wait_real(cx, "the catalog closing", |cx| {
        o.shell.read_with(cx, |s, _| s.pending_closings() == 0)
    });
}

#[gpui_kit::test]
fn a_broken_file_keeps_the_old_workspace_and_says_why(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    std::fs::write(
        o.fx.dir.path().join("dbento.toml"),
        "dbento = 1\n[connections.lite]\nengine = \"nope\"\n",
    )
    .unwrap();
    reload(cx, o.win);
    wait_real(cx, "the error", |cx| present(cx, o.win, "confirm-detail"));
    let detail = label_of(cx, o.win, "confirm-detail").unwrap();
    assert!(detail.contains("lite"), "{detail}");
    assert!(o.shell.read_with(cx, |s, _| {
        s.workspace().unwrap().connection("nopg").is_some()
    }));
}

#[gpui_kit::test]
fn the_history_panel_and_new_tabs_follow_the_reload(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    rewrite(&o.fx, "", AAA);
    cx.update_window(o.win, |_, w, cx| w.press("ctrl-shift-r", cx))
        .unwrap();
    cx.run_until_parked();
    let ws = o
        .shell
        .read_with(cx, |s, _| Arc::clone(s.workspace().unwrap()));
    assert!(ws.connection("aaa").is_some(), "the key reloads too");
    let panel = o
        .shell
        .read_with(cx, |s, _| s.history_panel().cloned().unwrap());
    let panel_ws = panel.read_with(cx, |p, _| Arc::clone(p.workspace()));
    assert!(Arc::ptr_eq(&panel_ws, &ws));
    support::open_connection_row(cx, o.win, "aaa");
    let spec_ws = o.shell.read_with(cx, |s, cx| {
        Arc::clone(&s.tab().unwrap().read(cx).model().read(cx).spec().workspace)
    });
    assert!(
        Arc::ptr_eq(&spec_ws, &ws),
        "a new tab runs on the new workspace"
    );
}
