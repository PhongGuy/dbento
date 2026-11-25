//! Child connections in the tab picker, the history filter, the palette and disconnect.
use crate::derived_tabs::with_shell;
use crate::support::{Started, label_of, palette_rows, started, wait_real};
use dbento_app::catalog::{CatalogData, CatalogQuery, CatalogReply};
use dbento_app::explorer::part_id;
use dbento_app::link::LinkState;
use dbento_app::palette::Mode;
use dbento_app::schema_tree::RowKind;
use dbento_core::{Relation, RelationKind, RelationList, SchemaInfo};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, ElementId, TestAppContext};

const PG: &str =
    "engine = \"postgres\"\nhost = \"127.0.0.1\"\nport = 9\ndatabase = \"x\"\nuser = \"x\"\n";

fn toml() -> String {
    format!(
        "dbento = 1\n[connections.lite]\nengine = \"sqlite\"\npath = \"app.db\"\n[connections.listed]\n{PG}databases = [\"app\", \"booking\"]\n[connections.srv]\n{PG}databases = \"all\"\n"
    )
}
fn click(cx: &mut TestAppContext, s: &Started, id: impl Into<ElementId>) {
    let id = id.into();
    cx.update_window(s.win, |_, w, cx| w.click(id, cx)).unwrap();
    cx.run_until_parked();
}
fn labels(cx: &mut TestAppContext, s: &Started, prefix: &'static str) -> Vec<String> {
    (0usize..)
        .map_while(|n| label_of(cx, s.win, (prefix, n)))
        .collect()
}
fn started_catalogs(cx: &mut TestAppContext, s: &Started) -> usize {
    s.sidebar(cx).read_with(cx, |sb, _| sb.catalogs_started())
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
/// Expands the `"all"` parent `srv`, lets its real listing fail, then answers it with `names`.
fn list_srv(cx: &mut TestAppContext, s: &Started, names: &[&str]) {
    click(cx, s, part_id("connection-toggle", "srv"));
    wait_real(cx, "the srv listing to fail", |cx| {
        s.sidebar(cx)
            .read_with(cx, |sb, _| sb.link("srv") != LinkState::Connecting)
    });
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
fn the_tab_picker_offers_listed_and_cached_children_without_listing(cx: &mut TestAppContext) {
    let s = started(cx, &toml());
    with_shell(cx, &s, |sh, w, cx| sh.open_connection_named("lite", w, cx));
    let before = started_catalogs(cx, &s);
    click(cx, &s, "query-connection");
    assert_eq!(
        labels(cx, &s, "query-connection-pick"),
        ["listed", "listed/app", "listed/booking", "lite", "srv"]
    );
    assert_eq!(
        started_catalogs(cx, &s),
        before,
        "the picker lists nothing itself"
    );
    cx.update_window(s.win, |_, w, cx| w.press("escape", cx))
        .unwrap();
    cx.run_until_parked();
    list_srv(cx, &s, &["one", "my.db"]);
    click(cx, &s, "query-connection");
    assert_eq!(
        labels(cx, &s, "query-connection-pick"),
        [
            "listed",
            "listed/app",
            "listed/booking",
            "lite",
            "srv",
            "srv/one"
        ],
        "an invalid database name is no connection"
    );
    click(cx, &s, ("query-connection-pick", 5usize));
    let conn = s.shell.read_with(cx, |sh, cx| {
        sh.tab()
            .unwrap()
            .read(cx)
            .model()
            .read(cx)
            .spec()
            .connection
            .clone()
    });
    assert_eq!(conn, "srv/one");
}

#[gpui_kit::test]
fn the_history_filter_offers_children(cx: &mut TestAppContext) {
    let s = started(cx, &toml());
    cx.update_window(s.win, |_, w, cx| w.press("ctrl-shift-h", cx))
        .unwrap();
    cx.run_until_parked();
    click(cx, &s, "history-connection");
    assert_eq!(
        labels(cx, &s, "history-filter"),
        [
            "all connections",
            "listed",
            "listed/app",
            "listed/booking",
            "lite",
            "srv"
        ]
    );
    click(cx, &s, ("history-filter", 2usize));
    assert_eq!(
        label_of(cx, s.win, "history-connection").as_deref(),
        Some("listed/app")
    );
}

#[gpui_kit::test]
fn the_palette_lists_a_childs_tables_and_offers_to_search_the_others(cx: &mut TestAppContext) {
    let s = started(cx, &toml());
    click(cx, &s, part_id("connection-toggle", "listed"));
    // The child's row is the first tree row; its own session cannot connect, so its
    // schemas fail first and are then answered as the runner would.
    click(cx, &s, ("tree-toggle", 0usize));
    wait_real(cx, "the child's session", |cx| {
        s.sidebar(cx)
            .read_with(cx, |sb, _| sb.link("listed/app") != LinkState::Connecting)
    });
    answer(
        cx,
        &s,
        "listed/app",
        CatalogQuery::Schemas,
        CatalogData::Schemas(vec![SchemaInfo::new("public")]),
    );
    // The schemas auto-open `public`, whose loads go to the failing runner; its replies must
    // land before ours, or a late failure replaces the tables we answer.
    wait_real(cx, "the runner's own replies", |cx| {
        s.sidebar(cx).read_with(cx, |sb, _| {
            let rows = sb.tree().rows("listed/app", 0, "");
            !rows.iter().any(|r| matches!(r.kind, RowKind::Loading))
        })
    });
    let public = CatalogQuery::Relations {
        schema: "public".into(),
    };
    let rels = RelationList::new(vec![Relation::new("orders", RelationKind::Table)], false);
    answer(cx, &s, "listed/app", public, CatalogData::Relations(rels));
    let shell = s.shell.clone();
    cx.update_window(s.win, |_, w, cx| {
        shell.update(cx, |sh, cx| sh.open_palette(Mode::All, w, cx))
    })
    .unwrap();
    cx.run_until_parked();
    cx.update_window(s.win, |_, w, cx| w.input("ord", cx))
        .unwrap();
    cx.run_until_parked();
    let has =
        |cx: &mut TestAppContext, label: &str| palette_rows(cx, s.win).iter().any(|r| r == label);
    wait_real(cx, "the child's table", |cx| {
        has(cx, "orders \u{b7} listed/app")
    });
    assert!(has(cx, "Search tables in listed/booking"));
}

#[gpui_kit::test]
fn disconnecting_a_parent_disconnects_its_children(cx: &mut TestAppContext) {
    let s = started(cx, &toml());
    click(cx, &s, part_id("connection-toggle", "listed"));
    click(cx, &s, ("tree-toggle", 0usize));
    let sidebar = s.sidebar(cx);
    wait_real(cx, "the child's catalog session", |cx| {
        sidebar.read_with(cx, |sb, _| sb.catalog_sessions()) == 1
    });
    with_shell(cx, &s, |sh, w, cx| {
        sh.open_sql("listed/app", "SELECT 1", w, cx)
    });
    let model = s
        .shell
        .read_with(cx, |sh, cx| sh.tab().unwrap().read(cx).model().clone());
    click(cx, &s, "run-all");
    wait_real(cx, "the child's failed connect", |cx| {
        model.read_with(cx, |m, _| matches!(m.link(), LinkState::Failed(_)))
    });
    with_shell(cx, &s, |sh, w, cx| sh.disconnect("listed", w, cx));
    assert_eq!(sidebar.read_with(cx, |sb, _| sb.catalog_sessions()), 0);
    assert_eq!(model.read_with(cx, |m, _| m.link()), LinkState::Idle);
}
