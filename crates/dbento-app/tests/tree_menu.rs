//! The explorer's catalog rows: Enter to open, hints, and their own menus.
use crate::support::{Opened, click_id, label_of, launched, present, wait_real};
use dbento_app::explorer::part_id;
use dbento_app::keymap::{EXPLORER_OPEN_KEYS, chip_keys};
use dbento_app::keys::KeyItem;
use dbento_app::schema_tree::{ObjectGroup, RowKind, catalog_entries, count_text, insert_sql};
use dbento_app::sidebar::{SidebarEvent, menu_icon, relation_tooltip};
use dbento_app::ui::Icons;
use dbento_core::{Engine, KeyKind, KeyTtl, ObjectRef, RelationKind, RelationRef};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AnyWindowHandle, AppContext as _, ElementId, TestAppContext};
use std::rc::Rc;

const T: &str = "CREATE TABLE Album(AlbumId INTEGER PRIMARY KEY, Title TEXT NOT NULL); \
                 INSERT INTO Album(Title) VALUES ('a'), ('b'), ('c'); \
                 CREATE VIEW Recent AS SELECT Title FROM Album;";
const MAIN_OPEN: [&str; 5] = [
    "lite.main",
    "lite.main/tables",
    "lite.main.Album",
    "lite.main/views",
    "lite.main.Recent",
];

fn tree(cx: &mut TestAppContext, win: AnyWindowHandle) -> Vec<String> {
    (0usize..)
        .map_while(|i| label_of(cx, win, ("tree-row", i)))
        .collect()
}
fn wait_tree(cx: &mut TestAppContext, win: AnyWindowHandle, want: &[&str]) {
    wait_real(cx, &format!("tree {want:?}"), |cx| tree(cx, win) == want);
}
/// The app on `setup` with `lite` and its `main` schema expanded.
fn expanded(cx: &mut TestAppContext, setup: &str, want: &[&str]) -> Opened {
    let o = launched(cx, setup, "");
    click_id(cx, o.win, part_id("connection-toggle", "lite"));
    wait_tree(cx, o.win, want);
    o
}

#[test]
fn a_relation_row_says_how_to_open_it() {
    assert_eq!(
        relation_tooltip(&RelationRef::new("main", "Album")),
        "main.Album \u{b7} Double-click or Enter to open"
    );
}

#[gpui_kit::test]
fn enter_on_a_selected_table_browses_it(cx: &mut TestAppContext) {
    let o = expanded(cx, T, &MAIN_OPEN);
    click_id(cx, o.win, ("tree-row", 2usize));
    cx.update_window(o.win, |_, w, cx| w.press("enter", cx))
        .unwrap();
    cx.run_until_parked();
    o.wait_browse(cx, "SELECT *\nFROM main.Album\nORDER BY AlbumId ASC");
}

#[gpui_kit::test]
fn enter_on_a_schema_row_opens_nothing(cx: &mut TestAppContext) {
    let o = expanded(cx, T, &MAIN_OPEN);
    click_id(cx, o.win, ("tree-row", 0usize));
    cx.update_window(o.win, |_, w, cx| w.press("enter", cx))
        .unwrap();
    cx.run_until_parked();
    assert!(o.shell.read_with(cx, |s, _| s.tab().is_none()));
    assert_eq!(tree(cx, o.win), MAIN_OPEN);
}

const CONNECTION_ITEMS: [&str; 5] = [
    "menu-new-connection",
    "menu-new-folder",
    "menu-rename",
    "menu-edit",
    "menu-delete",
];

fn right(cx: &mut TestAppContext, o: &Opened, id: impl Into<ElementId>) {
    let id = id.into();
    cx.update_window(o.win, |_, w, cx| w.right_click(id, cx))
        .unwrap();
    cx.run_until_parked();
}
fn clipboard(cx: &mut TestAppContext) -> Option<String> {
    cx.read_from_clipboard().and_then(|c| c.text())
}
fn menu(cx: &mut TestAppContext, o: &Opened, row: usize, item: &'static str) {
    right(cx, o, ("tree-row", row));
    click_id(cx, o.win, item);
}

#[gpui_kit::test]
fn each_row_kind_has_its_own_items(cx: &mut TestAppContext) {
    cx.update(|cx| {
        let ids = |k: RowKind| {
            catalog_entries(&k, Engine::Postgres, cx)
                .iter()
                .map(|e| e.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            ids(RowKind::Relation(RelationKind::Table)),
            [
                "tree-menu-open",
                "tree-menu-new-query",
                "tree-menu-copy-name",
                "tree-menu-copy-qualified",
                "tree-menu-count",
                "tree-menu-generate-select",
                "tree-menu-generate-insert",
                "tree-menu-refresh",
            ]
        );
        assert_eq!(
            ids(RowKind::Relation(RelationKind::View)),
            [
                "tree-menu-open",
                "tree-menu-new-query",
                "tree-menu-copy-name",
                "tree-menu-copy-qualified",
                "tree-menu-count",
                "tree-menu-generate-select",
                "tree-menu-view-definition",
                "tree-menu-refresh",
            ]
        );
        assert_eq!(
            ids(RowKind::Schema),
            [
                "tree-menu-new-query",
                "tree-menu-copy-name",
                "tree-menu-refresh"
            ]
        );
        assert_eq!(
            ids(RowKind::Kind(RelationKind::Table)),
            ["tree-menu-new-query", "tree-menu-refresh"]
        );
        let column = RowKind::Column {
            type_name: "INTEGER".into(),
            primary_key: true,
        };
        assert_eq!(
            ids(column),
            ["tree-menu-copy-name", "tree-menu-copy-qualified"]
        );
        let failed = RowKind::Failed {
            message: "boom".into(),
        };
        assert_eq!(ids(failed), ["tree-menu-retry", "tree-menu-copy-error"]);
        for k in [RowKind::Loading, RowKind::Empty, RowKind::Truncated] {
            assert!(
                catalog_entries(&k, Engine::Postgres, cx).is_empty(),
                "{k:?}"
            );
        }
        let table = catalog_entries(
            &RowKind::Relation(RelationKind::Table),
            Engine::Postgres,
            cx,
        );
        assert_eq!(
            table.iter().map(|e| e.label).collect::<Vec<_>>(),
            [
                "Open data",
                "New query on table",
                "Copy name",
                "Copy qualified name",
                "Count rows",
                "Generate SELECT",
                "Generate INSERT",
                "Refresh",
            ]
        );
        assert_eq!(
            catalog_entries(&RowKind::Relation(RelationKind::View), Engine::Postgres, cx)[1].label,
            "New query on view"
        );
        assert_eq!(table[0].keys, Some(EXPLORER_OPEN_KEYS.into()));
        assert!(table[1..].iter().all(|e| e.keys.is_none() && e.enabled));
    });
}

#[gpui_kit::test]
fn view_definition_shows_on_objects_views_and_sqlite_tables(cx: &mut TestAppContext) {
    cx.update(|cx| {
        let has = |k: RowKind, e: Engine| {
            catalog_entries(&k, e, cx)
                .iter()
                .any(|x| x.id == "tree-menu-view-definition")
        };
        assert!(!has(
            RowKind::Relation(RelationKind::Table),
            Engine::Postgres
        ));
        assert!(has(RowKind::Relation(RelationKind::Table), Engine::Sqlite));
        assert!(has(RowKind::Relation(RelationKind::Table), Engine::Mysql));
        assert!(has(RowKind::Relation(RelationKind::Table), Engine::Duckdb));
        assert!(has(RowKind::Relation(RelationKind::View), Engine::Postgres));
        assert!(has(
            RowKind::Relation(RelationKind::MaterializedView),
            Engine::Postgres
        ));
        assert!(!has(
            RowKind::Relation(RelationKind::ForeignTable),
            Engine::Postgres
        ));
        let object = RowKind::Object {
            group: ObjectGroup::Indexes,
            object: ObjectRef::relation(RelationRef::new("main", "t")),
            detail: String::new(),
        };
        let entries = catalog_entries(&object, Engine::Sqlite, cx);
        assert_eq!(
            entries.iter().map(|e| (e.id, e.label)).collect::<Vec<_>>(),
            [
                ("tree-menu-view-definition", "View definition"),
                ("tree-menu-copy-name", "Copy name"),
            ]
        );
        assert_eq!(entries[0].keys, Some(EXPLORER_OPEN_KEYS.into()));
        let group = catalog_entries(&RowKind::Group(ObjectGroup::Keys), Engine::Postgres, cx);
        assert_eq!(
            group.iter().map(|e| e.id).collect::<Vec<_>>(),
            ["tree-menu-refresh"]
        );
        assert_eq!(
            menu_icon("tree-menu-view-definition"),
            Some(Icons::FileText)
        );
    });
}

#[gpui_kit::test]
fn view_definition_on_a_sqlite_table_asks_for_its_create_table(cx: &mut TestAppContext) {
    let o = expanded(cx, T, &MAIN_OPEN);
    let sidebar = o.shell.read_with(cx, |s, _| s.sidebar().cloned().unwrap());
    let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let out = Rc::clone(&seen);
    cx.update(|cx| {
        cx.subscribe(&sidebar, move |_, e: &SidebarEvent, _| {
            if let SidebarEvent::ViewDefinition { connection, object } = e {
                out.borrow_mut().push((connection.clone(), object.clone()));
            }
        })
        .detach()
    });
    right(cx, &o, ("tree-row", 2usize));
    click_id(cx, o.win, "tree-menu-view-definition");
    assert_eq!(
        *seen.borrow(),
        [(
            "lite".to_owned(),
            ObjectRef::relation(RelationRef::new("main", "Album"))
        )]
    );
}

#[gpui_kit::test]
fn a_table_menu_offers_table_actions_only(cx: &mut TestAppContext) {
    let o = expanded(cx, T, &MAIN_OPEN);
    right(cx, &o, ("tree-row", 2usize));
    for id in [
        "tree-menu-open",
        "tree-menu-new-query",
        "tree-menu-copy-name",
        "tree-menu-copy-qualified",
        "tree-menu-generate-select",
        "tree-menu-refresh",
    ] {
        assert!(present(cx, o.win, id), "{id}");
    }
    for id in CONNECTION_ITEMS {
        assert!(!present(cx, o.win, id), "{id} on a table row");
    }
    assert_eq!(
        label_of(cx, o.win, "tree-menu-open-keys"),
        Some(chip_keys(EXPLORER_OPEN_KEYS))
    );
    assert!(!present(cx, o.win, "tree-menu-refresh-keys"));
}

#[gpui_kit::test]
fn schema_group_and_column_rows_have_their_own_menus(cx: &mut TestAppContext) {
    let o = expanded(cx, T, &MAIN_OPEN);
    right(cx, &o, ("tree-row", 0usize));
    assert!(present(cx, o.win, "tree-menu-copy-name"));
    assert!(!present(cx, o.win, "tree-menu-copy-qualified"));
    assert!(CONNECTION_ITEMS.iter().all(|id| !present(cx, o.win, *id)));
    click_id(cx, o.win, "tree-menu-copy-name");
    assert_eq!(clipboard(cx).as_deref(), Some("main"));
    right(cx, &o, ("tree-row", 1usize));
    assert!(present(cx, o.win, "tree-menu-new-query"));
    assert!(!present(cx, o.win, "tree-menu-copy-name"));
    click_id(cx, o.win, "tree-menu-refresh");
    wait_tree(cx, o.win, &MAIN_OPEN);
    click_id(cx, o.win, ("tree-toggle", 2usize));
    wait_tree(
        cx,
        o.win,
        &[
            "lite.main",
            "lite.main/tables",
            "lite.main.Album",
            "lite.main.Album.AlbumId",
            "lite.main.Album.Title",
            "lite.main.Album/keys",
            "lite.main/views",
            "lite.main.Recent",
        ],
    );
    right(cx, &o, ("tree-row", 3usize));
    assert!(!present(cx, o.win, "tree-menu-refresh"));
    assert!(CONNECTION_ITEMS.iter().all(|id| !present(cx, o.win, *id)));
    click_id(cx, o.win, "tree-menu-copy-qualified");
    assert_eq!(clipboard(cx).as_deref(), Some("main.Album.AlbumId"));
}

#[gpui_kit::test]
fn copy_and_generate_write_the_clipboard(cx: &mut TestAppContext) {
    let o = expanded(cx, T, &MAIN_OPEN);
    menu(cx, &o, 2, "tree-menu-copy-name");
    assert_eq!(clipboard(cx).as_deref(), Some("Album"));
    menu(cx, &o, 2, "tree-menu-copy-qualified");
    assert_eq!(clipboard(cx).as_deref(), Some("main.Album"));
    menu(cx, &o, 2, "tree-menu-generate-select");
    assert_eq!(clipboard(cx).as_deref(), Some("SELECT *\nFROM main.Album"));
    assert!(
        o.shell.read_with(cx, |s, _| s.tab().is_none()),
        "copies open nothing"
    );
}

#[gpui_kit::test]
fn open_data_from_the_menu_browses_the_table(cx: &mut TestAppContext) {
    let o = expanded(cx, T, &MAIN_OPEN);
    menu(cx, &o, 2, "tree-menu-open");
    o.wait_browse(cx, "SELECT *\nFROM main.Album\nORDER BY AlbumId ASC");
}

#[gpui_kit::test]
fn new_query_on_a_table_holds_its_select_unrun(cx: &mut TestAppContext) {
    let o = expanded(cx, T, &MAIN_OPEN);
    menu(cx, &o, 2, "tree-menu-new-query");
    let tab = o.shell.read_with(cx, |s, _| s.tab().cloned());
    let tab = tab.expect("a new tab");
    assert_eq!(
        tab.read_with(cx, |t, cx| t.sql(cx)),
        "SELECT *\nFROM main.Album"
    );
    assert!(o.model(cx).read_with(cx, |m, _| m.results().is_empty()));
    menu(cx, &o, 0, "tree-menu-new-query");
    assert_eq!(o.shell.read_with(cx, |s, _| s.tabs().len()), 2);
    assert_eq!(
        o.shell
            .read_with(cx, |s, cx| s.tab().unwrap().read(cx).sql(cx)),
        ""
    );
}

#[gpui_kit::test]
fn a_failed_row_offers_retry_and_its_error(cx: &mut TestAppContext) {
    let o = launched(cx, T, "");
    let db = o.fx.dir.path().join("data/app.db");
    std::fs::remove_file(&db).unwrap();
    click_id(cx, o.win, part_id("connection-toggle", "lite"));
    wait_tree(cx, o.win, &["lite:error"]);
    menu(cx, &o, 0, "tree-menu-copy-error");
    assert!(clipboard(cx).is_some_and(|t| !t.is_empty()));
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute_batch(T)
        .unwrap();
    menu(cx, &o, 0, "tree-menu-retry");
    wait_tree(cx, o.win, &MAIN_OPEN);
}

#[gpui_kit::test]
fn a_status_row_has_no_menu(cx: &mut TestAppContext) {
    let o = expanded(cx, "", &["lite.main", "lite.main:empty"]);
    right(cx, &o, ("tree-row", 1usize));
    assert!(!present(cx, o.win, "tree-menu-refresh"));
    assert!(CONNECTION_ITEMS.iter().all(|id| !present(cx, o.win, *id)));
}

#[test]
fn insert_and_count_text_read_naturally() {
    let rel = RelationRef::new("main", "Album");
    assert_eq!(
        insert_sql(Engine::Sqlite, &rel, &["AlbumId", "Title"]),
        "INSERT INTO main.Album (AlbumId, Title)\nVALUES (NULL, NULL)"
    );
    assert_eq!(
        insert_sql(
            Engine::Postgres,
            &RelationRef::new("shop", "Order"),
            &["id", "Total"]
        ),
        "INSERT INTO shop.\"Order\" (id, \"Total\")\nVALUES (NULL, NULL)"
    );
    assert_eq!(count_text(&rel, 1), "main.Album: 1 row");
    assert_eq!(count_text(&rel, 12345), "main.Album: 12,345 rows");
}

#[gpui_kit::test]
fn count_rows_shows_a_notice(cx: &mut TestAppContext) {
    let o = expanded(cx, T, &MAIN_OPEN);
    menu(cx, &o, 2, "tree-menu-count");
    wait_real(cx, "the count", |cx| {
        label_of(cx, o.win, "notice").as_deref() == Some("main.Album: 3 rows")
    });
    assert_eq!(tree(cx, o.win), MAIN_OPEN, "the count changes no level");
}

#[gpui_kit::test]
fn generate_insert_loads_the_columns_first(cx: &mut TestAppContext) {
    let o = expanded(cx, T, &MAIN_OPEN);
    menu(cx, &o, 2, "tree-menu-generate-insert");
    wait_real(cx, "the INSERT", |cx| {
        clipboard(cx).as_deref()
            == Some("INSERT INTO main.Album (AlbumId, Title)\nVALUES (NULL, NULL)")
    });
    assert_eq!(tree(cx, o.win), MAIN_OPEN, "Album stays collapsed");
    wait_real(cx, "the INSERT notice", |cx| {
        label_of(cx, o.win, "notice").as_deref() == Some("INSERT for main.Album copied")
    });
}

#[gpui_kit::test]
fn generate_insert_with_the_columns_loaded_copies_with_a_notice(cx: &mut TestAppContext) {
    let o = expanded(cx, T, &MAIN_OPEN);
    click_id(cx, o.win, ("tree-toggle", 2usize));
    wait_tree(
        cx,
        o.win,
        &[
            "lite.main",
            "lite.main/tables",
            "lite.main.Album",
            "lite.main.Album.AlbumId",
            "lite.main.Album.Title",
            "lite.main.Album/keys",
            "lite.main/views",
            "lite.main.Recent",
        ],
    );
    menu(cx, &o, 2, "tree-menu-generate-insert");
    assert_eq!(
        clipboard(cx).as_deref(),
        Some("INSERT INTO main.Album (AlbumId, Title)\nVALUES (NULL, NULL)")
    );
    assert_eq!(
        label_of(cx, o.win, "notice").as_deref(),
        Some("INSERT for main.Album copied")
    );
}

#[gpui_kit::test]
fn a_refresh_before_the_columns_land_still_copies_the_insert_once(cx: &mut TestAppContext) {
    let o = expanded(cx, T, &MAIN_OPEN);
    right(cx, &o, ("tree-row", 2usize));
    let sidebar = o.shell.read_with(cx, |s, _| s.sidebar().cloned().unwrap());
    // The click sends the columns load and the refresh lands in the same turn, so the reply
    // can only arrive under the old generation.
    cx.update_window(o.win, |_, w, cx| {
        w.click("tree-menu-generate-insert", cx);
        sidebar.update(cx, |s, cx| s.refresh(cx));
    })
    .unwrap();
    let insert = "INSERT INTO main.Album (AlbumId, Title)\nVALUES (NULL, NULL)";
    wait_real(cx, "the INSERT after a refresh", |cx| {
        clipboard(cx).as_deref() == Some(insert)
    });
    cx.write_to_clipboard(gpui_kit::ClipboardItem::new_string("mine".into()));
    click_id(cx, o.win, ("tree-toggle", 2usize));
    wait_tree(
        cx,
        o.win,
        &[
            "lite.main",
            "lite.main/tables",
            "lite.main.Album",
            "lite.main.Album.AlbumId",
            "lite.main.Album.Title",
            "lite.main.Album/keys",
            "lite.main/views",
            "lite.main.Recent",
        ],
    );
    assert_eq!(clipboard(cx).as_deref(), Some("mine"), "copied only once");
}

// The menu builds after the mouse down; a refresh in between must not retarget it.
#[gpui_kit::test]
fn a_catalog_menu_opened_across_a_refresh_still_names_its_row(cx: &mut TestAppContext) {
    let o = expanded(cx, T, &MAIN_OPEN);
    let sidebar = o.shell.read_with(cx, |s, _| s.sidebar().cloned().unwrap());
    cx.update_window(o.win, |_, w, cx| {
        w.right_click(("tree-row", 2usize), cx);
        sidebar.update(cx, |s, cx| s.refresh(cx));
    })
    .unwrap();
    cx.run_until_parked();
    click_id(cx, o.win, "tree-menu-copy-qualified");
    assert_eq!(clipboard(cx).as_deref(), Some("main.Album"));
}

#[gpui_kit::test]
fn surrealdb_menus_offer_no_generated_sql(cx: &mut TestAppContext) {
    cx.update(|cx| {
        let ids = |k: RowKind| {
            catalog_entries(&k, Engine::Surrealdb, cx)
                .iter()
                .map(|e| e.id)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            ids(RowKind::Relation(RelationKind::Table)),
            [
                "tree-menu-open",
                "tree-menu-new-query",
                "tree-menu-copy-name",
                "tree-menu-generate-select",
                "tree-menu-refresh",
            ]
        );
        assert_eq!(
            ids(RowKind::Column {
                type_name: "string".into(),
                primary_key: false,
            }),
            ["tree-menu-copy-name"]
        );
    });
}

#[gpui_kit::test]
fn mongodb_menus_generate_find_and_no_sql(cx: &mut TestAppContext) {
    cx.update(|cx| {
        let entries = |k: RowKind| catalog_entries(&k, Engine::Mongodb, cx);
        let table = entries(RowKind::Relation(RelationKind::Table));
        let ids: Vec<_> = table.iter().map(|e| e.id).collect();
        assert_eq!(
            ids,
            [
                "tree-menu-open",
                "tree-menu-new-query",
                "tree-menu-copy-name",
                "tree-menu-generate-select",
                "tree-menu-refresh",
            ]
        );
        let find = table
            .iter()
            .find(|e| e.id == "tree-menu-generate-select")
            .unwrap();
        assert_eq!(find.label.to_string(), "Generate find");
        let view = entries(RowKind::Relation(RelationKind::View));
        assert!(view.iter().all(|e| e.id != "tree-menu-view-definition"));
        let pg = catalog_entries(
            &RowKind::Relation(RelationKind::Table),
            Engine::Postgres,
            cx,
        );
        let label = &pg
            .iter()
            .find(|e| e.id == "tree-menu-generate-select")
            .unwrap()
            .label;
        assert_eq!(label.to_string(), "Generate SELECT");
    });
}

#[gpui_kit::test]
fn redis_rows_offer_key_actions_and_no_sql(cx: &mut TestAppContext) {
    cx.update(|cx| {
        let ids = |k: RowKind, e: Engine| -> Vec<&'static str> {
            catalog_entries(&k, e, cx).iter().map(|m| m.id).collect()
        };
        assert_eq!(
            ids(RowKind::Schema, Engine::Redis),
            [
                "tree-menu-new-query",
                "tree-menu-copy-name",
                "tree-menu-filter-keys",
                "tree-menu-refresh"
            ]
        );
        assert!(!ids(RowKind::Schema, Engine::Postgres).contains(&"tree-menu-filter-keys"));
        let key = KeyItem {
            name: b"user:1".to_vec(),
            kind: KeyKind::Hash,
            ttl: KeyTtl::Persistent,
        };
        let entries = catalog_entries(&RowKind::Key(key.clone()), Engine::Redis, cx);
        assert_eq!(entries[0].label.to_string(), "Open value");
        assert_eq!(
            ids(RowKind::Key(key), Engine::Redis),
            ["tree-menu-open-key", "tree-menu-copy-name"]
        );
        assert_eq!(
            ids(
                RowKind::KeyFolder {
                    prefix: b"user:".to_vec(),
                    keys: 2
                },
                Engine::Redis
            ),
            ["tree-menu-copy-name"]
        );
        assert!(ids(RowKind::LoadMore, Engine::Redis).is_empty());
        let table = ids(RowKind::Relation(RelationKind::Table), Engine::Redis);
        for id in [
            "tree-menu-count",
            "tree-menu-generate-insert",
            "tree-menu-copy-qualified",
        ] {
            assert!(!table.contains(&id), "{id}");
        }
    });
}
