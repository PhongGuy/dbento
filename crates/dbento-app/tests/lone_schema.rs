//! A connection with a single schema opens straight to its tables, once.
use crate::support::{click_id, label_of, launched, reload, rewrite, wait_real};
use dbento_app::explorer::{connection_id, part_id};
use dbento_app::schema_tree::lone_schema;
use dbento_core::SchemaInfo;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AnyWindowHandle, AppContext as _, TestAppContext};

const T: &str = "CREATE TABLE t(id INTEGER);";
const OPEN: [&str; 3] = ["lite.main", "lite.main/tables", "lite.main.t"];

fn tree(cx: &mut TestAppContext, win: AnyWindowHandle) -> Vec<String> {
    (0usize..)
        .map_while(|i| label_of(cx, win, ("tree-row", i)))
        .collect()
}

#[test]
fn only_a_single_schema_is_lone() {
    let one = [SchemaInfo::new("main")];
    let two = [SchemaInfo::new("main"), SchemaInfo::new("aux")];
    assert_eq!(lone_schema(&one), Some("main"));
    assert_eq!(lone_schema(&two), None);
    assert_eq!(lone_schema(&[]), None);
}

#[gpui_kit::test]
fn a_lone_schema_opens_itself(cx: &mut TestAppContext) {
    let o = launched(cx, T, "");
    click_id(cx, o.win, part_id("connection-toggle", "lite"));
    wait_real(cx, "main's table", |cx| tree(cx, o.win) == OPEN);
}

#[gpui_kit::test]
fn a_collapsed_lone_schema_stays_collapsed_after_refresh(cx: &mut TestAppContext) {
    let o = launched(cx, T, "");
    click_id(cx, o.win, part_id("connection-toggle", "lite"));
    wait_real(cx, "main's table", |cx| tree(cx, o.win) == OPEN);
    click_id(cx, o.win, ("tree-toggle", 0usize));
    assert_eq!(tree(cx, o.win), ["lite.main"]);
    let sidebar = o.shell.read_with(cx, |s, _| s.sidebar().cloned().unwrap());
    click_id(cx, o.win, "schema-refresh");
    wait_real(cx, "the reloaded schemas", |cx| {
        sidebar.read_with(cx, |s, _| s.tree().schemas("lite").is_some())
    });
    cx.run_until_parked();
    assert_eq!(tree(cx, o.win), ["lite.main"]);
}

#[gpui_kit::test]
fn after_disconnect_the_lone_schema_opens_again(cx: &mut TestAppContext) {
    let o = launched(cx, T, "");
    click_id(cx, o.win, part_id("connection-toggle", "lite"));
    wait_real(cx, "main's table", |cx| tree(cx, o.win) == OPEN);
    click_id(cx, o.win, ("tree-toggle", 0usize));
    cx.update_window(o.win, |_, w, cx| w.right_click(connection_id("lite"), cx))
        .unwrap();
    cx.run_until_parked();
    click_id(cx, o.win, "menu-disconnect");
    assert!(tree(cx, o.win).is_empty());
    click_id(cx, o.win, part_id("connection-toggle", "lite"));
    wait_real(cx, "main's table again", |cx| tree(cx, o.win) == OPEN);
}

#[gpui_kit::test]
fn a_deleted_then_readded_connection_opens_its_lone_schema_again(cx: &mut TestAppContext) {
    let o = launched(cx, T, "");
    click_id(cx, o.win, part_id("connection-toggle", "lite"));
    wait_real(cx, "main's table", |cx| tree(cx, o.win) == OPEN);
    let toml = o.fx.dir.path().join("dbento.toml");
    let full = std::fs::read_to_string(&toml).unwrap();
    std::fs::write(
        &toml,
        full.replace("[connections.lite]", "[connections.gone]"),
    )
    .unwrap();
    reload(cx, o.win);
    rewrite(&o.fx, "", "");
    reload(cx, o.win);
    click_id(cx, o.win, part_id("connection-toggle", "lite"));
    wait_real(cx, "main's table again", |cx| tree(cx, o.win) == OPEN);
}
