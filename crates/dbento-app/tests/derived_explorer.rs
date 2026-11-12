//! The explorer's databases level: listing, invalid rows, child sessions and reloads.
use crate::derived_tabs::{tab_conns, with_shell};
use crate::support::{Started, label_of, present, reload, started, wait_real};
use dbento_app::catalog::{CatalogData, CatalogQuery, CatalogReply};
use dbento_app::explorer::part_id;
use dbento_app::schema_tree::NodeKey;
use dbento_core::{Relation, RelationKind, RelationList, RelationRef, SchemaInfo};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AnyWindowHandle, AppContext as _, ElementId, TestAppContext};

const PG: &str =
    "engine = \"postgres\"\nhost = \"127.0.0.1\"\nport = 9\ndatabase = \"x\"\nuser = \"x\"\n";
fn toml(lite: &str, srv: &str) -> String {
    format!(
        "dbento = 1\n[connections.lite]\nengine = \"sqlite\"\npath = \"app.db\"\n{lite}\n[connections.listed]\n{PG}databases = [\"app\", \"booking\"]\n[connections.srv]\n{PG}databases = \"all\"\n{srv}\n"
    )
}
fn click(cx: &mut TestAppContext, win: AnyWindowHandle, id: impl Into<ElementId>) {
    let id = id.into();
    cx.update_window(win, |_, w, cx| w.click(id, cx)).unwrap();
    cx.run_until_parked();
}
fn tree(cx: &mut TestAppContext, win: AnyWindowHandle) -> Vec<String> {
    (0usize..)
        .map_while(|i| label_of(cx, win, ("tree-row", i)))
        .collect()
}
fn srv_rows(cx: &mut TestAppContext, win: AnyWindowHandle) -> Vec<String> {
    tree(cx, win)
        .into_iter()
        .filter(|r| r.starts_with("srv"))
        .collect()
}
fn wait_srv(cx: &mut TestAppContext, win: AnyWindowHandle, want: &[&str]) {
    wait_real(cx, &format!("srv rows {want:?}"), |cx| {
        srv_rows(cx, win) == want
    });
}
/// Expands `srv`, waits for its real listing to fail (nothing listens on port 9), then
/// answers it with `names` as the runner would.
fn listed_srv(cx: &mut TestAppContext, s: &Started, names: &[&str]) {
    click(cx, s.win, part_id("connection-toggle", "srv"));
    wait_srv(cx, s.win, &["srv:error"]);
    let names = names.iter().map(|n| (*n).to_owned()).collect();
    answer(
        cx,
        s,
        "srv",
        CatalogQuery::Databases,
        CatalogData::Databases(names),
    );
}

#[gpui_kit::test]
fn an_all_parent_lists_children_and_an_invalid_name_cannot_expand(cx: &mut TestAppContext) {
    let s = started(cx, &toml("", ""));
    listed_srv(cx, &s, &["app", "my.db"]);
    wait_srv(cx, s.win, &["srv/app", "srv/my.db:invalid"]);
    assert!(present(cx, s.win, ("tree-toggle", 0usize)));
    assert!(
        !present(cx, s.win, ("tree-toggle", 1usize)),
        "an invalid name has no chevron"
    );
    cx.update_window(s.win, |_, w, cx| w.double_click(("tree-row", 1usize), cx))
        .unwrap();
    cx.run_until_parked();
    assert!(
        tab_conns(cx, &s).is_empty(),
        "an invalid name opens nothing"
    );
}

#[gpui_kit::test]
fn a_child_expands_on_its_own_session_and_opens_a_tab(cx: &mut TestAppContext) {
    let s = started(cx, &toml("", ""));
    listed_srv(cx, &s, &["app"]);
    wait_srv(cx, s.win, &["srv/app"]);
    let sidebar = s.sidebar(cx);
    let before = sidebar.read_with(cx, |sb, _| sb.catalogs_started());
    click(cx, s.win, ("tree-toggle", 0usize));
    wait_srv(cx, s.win, &["srv/app", "srv/app:error"]);
    assert_eq!(
        sidebar.read_with(cx, |sb, _| sb.catalogs_started()),
        before + 1
    );
    click(cx, s.win, ("tree-row", 0usize));
    assert_eq!(
        sidebar.read_with(cx, |sb, _| sb.selected_node()),
        Some(NodeKey::Connection("srv/app".into()))
    );
    cx.update_window(s.win, |_, w, cx| w.double_click(("tree-row", 0usize), cx))
        .unwrap();
    cx.run_until_parked();
    assert_eq!(tab_conns(cx, &s), ["srv/app"]);
}

/// Answers `conn`'s catalog runner with `query` and `data`, as the runner would.
fn answer(
    cx: &mut TestAppContext,
    s: &Started,
    conn: &str,
    query: CatalogQuery,
    data: CatalogData,
) {
    let sidebar = s.sidebar(cx);
    let generation = sidebar.read_with(cx, |sb, _| sb.tree().generation());
    let reply = CatalogReply {
        generation,
        query,
        result: Ok(data),
    };
    sidebar.update(cx, |sb, cx| sb.apply_replies(conn, vec![reply], cx));
    cx.run_until_parked();
}

#[gpui_kit::test]
fn a_table_under_a_child_selects_and_opens_on_the_child(cx: &mut TestAppContext) {
    let s = started(cx, &toml("", ""));
    listed_srv(cx, &s, &["app"]);
    click(cx, s.win, ("tree-toggle", 0usize));
    wait_srv(cx, s.win, &["srv/app", "srv/app:error"]);
    answer(
        cx,
        &s,
        "srv/app",
        CatalogQuery::Schemas,
        CatalogData::Schemas(vec![SchemaInfo::new("public")]),
    );
    // A lone schema opens itself on the child as on any connection. The child's runner cannot
    // connect either, so its relations level fails first.
    wait_real(cx, "the child's relations error", |cx| {
        srv_rows(cx, s.win)
            .iter()
            .any(|r| r.starts_with("srv/app.public") && r.ends_with(":error"))
    });
    let public = CatalogQuery::Relations {
        schema: "public".into(),
    };
    let rels = RelationList::new(vec![Relation::new("t", RelationKind::Table)], false);
    answer(cx, &s, "srv/app", public, CatalogData::Relations(rels));
    wait_real(cx, "the child's table", |cx| {
        srv_rows(cx, s.win).iter().any(|r| r == "srv/app.public.t")
    });
    let ix = tree(cx, s.win)
        .iter()
        .position(|r| r == "srv/app.public.t")
        .unwrap();
    click(cx, s.win, ("tree-row", ix));
    let sidebar = s.sidebar(cx);
    assert_eq!(
        sidebar.read_with(cx, |sb, _| sb.selected_node()),
        Some(NodeKey::Relation {
            conn: "srv/app".into(),
            rel: RelationRef::new("public", "t")
        })
    );
    cx.update_window(s.win, |_, w, cx| w.double_click(("tree-row", ix), cx))
        .unwrap();
    cx.run_until_parked();
    assert_eq!(tab_conns(cx, &s), ["srv/app"]);
}

#[gpui_kit::test]
fn a_listed_parent_starts_no_session(cx: &mut TestAppContext) {
    let s = started(cx, &toml("", ""));
    click(cx, s.win, part_id("connection-toggle", "listed"));
    wait_real(cx, "the list", |cx| {
        tree(cx, s.win) == ["listed/app", "listed/booking"]
    });
    assert_eq!(
        s.sidebar(cx).read_with(cx, |sb, _| sb.catalogs_started()),
        0
    );
}

#[gpui_kit::test]
fn a_reload_keeps_the_listing_until_the_parent_changes(cx: &mut TestAppContext) {
    let s = started(cx, &toml("", ""));
    click(cx, s.win, part_id("connection-toggle", "lite"));
    listed_srv(cx, &s, &["app"]);
    wait_srv(cx, s.win, &["srv/app"]);
    // Another connection's change refreshes every expanded level; srv keeps its list.
    std::fs::write(
        s.dir.path().join("dbento.toml"),
        toml("read_only = true", ""),
    )
    .unwrap();
    reload(cx, s.win);
    wait_srv(cx, s.win, &["srv/app"]);
    let sidebar = s.sidebar(cx);
    assert_eq!(
        sidebar.read_with(cx, |sb, _| sb
            .tree()
            .databases("srv")
            .map(<[String]>::to_vec)),
        Some(vec!["app".to_owned()])
    );
    // srv's own change drops the list and lists again, which fails on port 9.
    std::fs::write(
        s.dir.path().join("dbento.toml"),
        toml("read_only = true", "read_only = true"),
    )
    .unwrap();
    reload(cx, s.win);
    wait_srv(cx, s.win, &["srv:error"]);
}

#[gpui_kit::test]
fn dropping_databases_forgets_the_children(cx: &mut TestAppContext) {
    let s = started(cx, &toml("", ""));
    listed_srv(cx, &s, &["app"]);
    click(cx, s.win, ("tree-toggle", 0usize));
    wait_srv(cx, s.win, &["srv/app", "srv/app:error"]);
    let plain = toml("", "").replace("databases = \"all\"\n", "");
    std::fs::write(s.dir.path().join("dbento.toml"), plain).unwrap();
    reload(cx, s.win);
    wait_real(cx, "no child rows", |cx| {
        !srv_rows(cx, s.win).iter().any(|r| r.starts_with("srv/"))
    });
    let sidebar = s.sidebar(cx);
    assert!(!sidebar.read_with(cx, |sb, _| {
        sb.tree()
            .is_expanded(&NodeKey::Connection("srv/app".into()))
    }));
}

#[gpui_kit::test]
fn a_new_tab_opens_on_a_selected_child(cx: &mut TestAppContext) {
    let s = started(cx, &toml("", ""));
    listed_srv(cx, &s, &["app"]);
    wait_srv(cx, s.win, &["srv/app"]);
    click(cx, s.win, ("tree-row", 0usize));
    with_shell(cx, &s, |sh, w, cx| sh.new_tab(w, cx));
    assert_eq!(tab_conns(cx, &s), ["srv/app"]);
}

#[gpui_kit::test]
fn an_expanded_connection_that_becomes_a_parent_lists_its_databases(cx: &mut TestAppContext) {
    // The dialog's Save takes this path too: only `databases` changes, so the session stays.
    let plain = format!("dbento = 1\n[connections.srv]\n{PG}");
    let s = started(cx, &plain);
    click(cx, s.win, part_id("connection-toggle", "srv"));
    wait_srv(cx, s.win, &["srv:error"]);
    let parent = format!("{plain}databases = \"all\"\n");
    std::fs::write(s.dir.path().join("dbento.toml"), parent).unwrap();
    reload(cx, s.win);
    // Nothing listens on port 9, so a listing that starts fails, and one that never starts
    // would leave the row loading.
    wait_srv(cx, s.win, &["srv:error"]);
    answer(
        cx,
        &s,
        "srv",
        CatalogQuery::Databases,
        CatalogData::Databases(vec!["app".into()]),
    );
    wait_srv(cx, s.win, &["srv/app"]);
    std::fs::write(s.dir.path().join("dbento.toml"), &plain).unwrap();
    reload(cx, s.win);
    wait_srv(cx, s.win, &["srv:error"]);
}
