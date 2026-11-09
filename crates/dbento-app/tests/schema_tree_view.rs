//! The explorer's schema tree: select on click, expand with the chevron, load, retry, and close
//! sessions on quit.
use crate::support;
use std::cell::RefCell;
use std::rc::Rc;

use crate::support::{Fx, fixture, init, init_tokio, label_of, present, services, wait_real};
use dbento_app::app::start;
use dbento_app::catalog::{CatalogQuery, CatalogReply};
use dbento_app::explorer::{connection_id, part_id};
use dbento_app::schema_tree::{NodeKey, ObjectGroup, RowKind};
use dbento_app::shell::Shell;
use dbento_app::sidebar::SidebarEvent;
use dbento_core::{Definition, ObjectRef, RelationKind, RelationRef};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{
    AnyWindowHandle, AppContext as _, ElementId, Entity, TestAppContext, VisualTestContext,
};

const T: &str = "CREATE TABLE Album(AlbumId INTEGER PRIMARY KEY, Title TEXT NOT NULL); \
                 CREATE VIEW Recent AS SELECT Title FROM Album; \
                 INSERT INTO Album(Title) VALUES ('a'), ('b'), ('c');";
const MAIN_OPEN: [&str; 5] = [
    "lite.main",
    "lite.main/tables",
    "lite.main.Album",
    "lite.main/views",
    "lite.main.Recent",
];

fn started(
    cx: &mut TestAppContext,
    setup: &str,
) -> (Fx, tempfile::TempDir, AnyWindowHandle, Entity<Shell>) {
    init(cx);
    init_tokio(cx);
    let (fx, files) = (fixture(setup, ""), tempfile::tempdir().unwrap());
    let (win, shell) = cx
        .update(|cx| start(services(files.path()), Some(fx.dir.path().into()), cx))
        .unwrap();
    assert!(present(cx, win, "schema-tree"));
    assert!(present(cx, win, "connection-list"));
    (fx, files, win, shell)
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
fn wait_tree(cx: &mut TestAppContext, win: AnyWindowHandle, want: &[&str]) {
    wait_real(cx, &format!("tree {want:?}"), |cx| tree(cx, win) == want);
}

#[gpui_kit::test]
fn the_chevron_walks_down_to_columns_without_opening_a_tab(cx: &mut TestAppContext) {
    let (_fx, _files, win, shell) = started(cx, T);
    assert!(tree(cx, win).is_empty());
    click(cx, win, part_id("connection-toggle", "lite"));
    wait_tree(cx, win, &MAIN_OPEN);
    assert!(shell.read_with(cx, |s, _| s.tab().is_none()));
    click(cx, win, ("tree-toggle", 2usize));
    wait_tree(
        cx,
        win,
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
    // Collapsing the connection hides its rows; expanding shows the cache at once.
    click(cx, win, part_id("connection-toggle", "lite"));
    assert!(tree(cx, win).is_empty());
    click(cx, win, part_id("connection-toggle", "lite"));
    assert_eq!(tree(cx, win).len(), 8);
    let sidebar = shell.read_with(cx, |s, _| s.sidebar().cloned().unwrap());
    assert_eq!(
        sidebar.read_with(cx, |s, _| s
            .tree()
            .relations("lite", "main")
            .map(|l| l.items().len())),
        Some(2)
    );
}

#[gpui_kit::test]
fn a_failed_load_shows_an_error_row_and_retry_reloads(cx: &mut TestAppContext) {
    let (fx, _files, win, _shell) = started(cx, T);
    let db = fx.dir.path().join("data/app.db");
    std::fs::remove_file(&db).unwrap();
    click(cx, win, part_id("connection-toggle", "lite"));
    wait_tree(cx, win, &["lite:error"]);
    assert!(present(cx, win, ("tree-retry", 0usize)));
    rusqlite::Connection::open(&db)
        .unwrap()
        .execute_batch(T)
        .unwrap();
    click(cx, win, ("tree-retry", 0usize));
    wait_tree(cx, win, &MAIN_OPEN);
}

#[gpui_kit::test]
fn a_connection_without_a_secret_shows_its_error(cx: &mut TestAppContext) {
    let (_fx, _files, win, _shell) = started(cx, T);
    click(cx, win, part_id("connection-toggle", "nopg"));
    wait_tree(cx, win, &["nopg:error"]);
}

#[gpui_kit::test]
fn closing_the_window_closes_catalog_sessions(cx: &mut TestAppContext) {
    let (_fx, _files, win, _shell) = started(cx, T);
    click(cx, win, part_id("connection-toggle", "lite"));
    wait_tree(cx, win, &MAIN_OPEN);
    // A catalog session is open and no tab is: the close waits for it, then goes through.
    assert!(!VisualTestContext::from_window(win, cx).simulate_close());
    wait_real(cx, "the window to close", |cx| {
        cx.update(|cx| cx.windows().is_empty())
    });
}

fn expanded_main(
    cx: &mut TestAppContext,
) -> (Fx, tempfile::TempDir, AnyWindowHandle, Entity<Shell>) {
    let s = started(cx, T);
    click(cx, s.2, part_id("connection-toggle", "lite"));
    wait_tree(cx, s.2, &MAIN_OPEN);
    s
}

#[gpui_kit::test]
fn a_single_click_selects_a_schema_without_expanding_it(cx: &mut TestAppContext) {
    let (_fx, _files, win, shell) = started(cx, T);
    click(cx, win, part_id("connection-toggle", "lite"));
    wait_tree(cx, win, &MAIN_OPEN);
    click(cx, win, ("tree-toggle", 0usize));
    wait_tree(cx, win, &["lite.main"]);
    click(cx, win, ("tree-row", 0usize));
    cx.run_until_parked();
    assert_eq!(tree(cx, win), ["lite.main"]);
    let sidebar = shell.read_with(cx, |s, _| s.sidebar().cloned().unwrap());
    assert_eq!(
        sidebar.read_with(cx, |s, _| s.selected_node()),
        Some(NodeKey::Schema {
            conn: "lite".into(),
            schema: "main".into()
        })
    );
    assert_eq!(sidebar.read_with(cx, |s, _| s.selected()), Some(0));
    click(cx, win, ("tree-toggle", 0usize));
    wait_tree(cx, win, &MAIN_OPEN);
}

#[gpui_kit::test]
fn a_single_click_on_a_relation_selects_it_and_opens_nothing(cx: &mut TestAppContext) {
    let (_fx, _files, win, shell) = expanded_main(cx);
    click(cx, win, ("tree-row", 2usize));
    cx.run_until_parked();
    assert_eq!(tree(cx, win), MAIN_OPEN);
    assert!(shell.read_with(cx, |s, _| s.tab().is_none()));
    let sidebar = shell.read_with(cx, |s, _| s.sidebar().cloned().unwrap());
    assert!(matches!(
        sidebar.read_with(cx, |s, _| s.selected_node()),
        Some(NodeKey::Relation { .. })
    ));
}

#[gpui_kit::test]
fn double_clicking_a_catalog_chevron_toggles_once(cx: &mut TestAppContext) {
    let (_fx, _files, win, shell) = expanded_main(cx);
    cx.update_window(win, |_, w, cx| w.double_click(("tree-toggle", 2usize), cx))
        .unwrap();
    wait_real(cx, "the columns", |cx| tree(cx, win).len() == 8);
    assert!(shell.read_with(cx, |s, _| s.tab().is_none()));
}

#[gpui_kit::test]
fn the_filter_narrows_loaded_names(cx: &mut TestAppContext) {
    let (_fx, _files, win, shell) = expanded_main(cx);
    let filter = shell.read_with(cx, |s, cx| {
        s.sidebar().unwrap().read(cx).filter_state().clone()
    });
    assert!(present(cx, win, "schema-filter"));
    cx.update_window(win, |_, w, cx| {
        filter.update(cx, |f, cx| f.focus(w, cx));
        w.input("rec", cx);
    })
    .unwrap();
    wait_tree(
        cx,
        win,
        &["lite.main", "lite.main/views", "lite.main.Recent"],
    );
}

#[gpui_kit::test]
fn refresh_reloads_what_is_expanded(cx: &mut TestAppContext) {
    let (fx, _files, win, _shell) = expanded_main(cx);
    rusqlite::Connection::open(fx.dir.path().join("data/app.db"))
        .unwrap()
        .execute_batch("CREATE TABLE Zed(x)")
        .unwrap();
    click(cx, win, "schema-refresh");
    wait_tree(
        cx,
        win,
        &[
            "lite.main",
            "lite.main/tables",
            "lite.main.Album",
            "lite.main.Zed",
            "lite.main/views",
            "lite.main.Recent",
        ],
    );
}

#[gpui_kit::test]
fn double_clicking_a_relation_opens_and_runs_its_select(cx: &mut TestAppContext) {
    let (_fx, _files, win, shell) = expanded_main(cx);
    cx.update_window(win, |_, w, cx| w.double_click(("tree-row", 2usize), cx))
        .unwrap();
    wait_real(cx, "the tab", |cx| {
        shell.read_with(cx, |s, _| s.tab().is_some())
    });
    let model = shell.read_with(cx, |s, cx| s.tab().unwrap().read(cx).model().clone());
    wait_real(cx, "the result", |cx| {
        model.read_with(cx, |m, _| {
            m.results().first().is_some_and(|r| r.store.end().is_some())
        })
    });
    model.read_with(cx, |m, _| {
        assert_eq!(
            m.results()[0].sql,
            "SELECT *\nFROM main.Album\nORDER BY AlbumId ASC"
        );
        assert_eq!(m.results()[0].store.rows(), 3);
    });
    // Neither click of the pair expands the relation; only its chevron does.
    assert_eq!(tree(cx, win), MAIN_OPEN);
    let sidebar = shell.read_with(cx, |s, _| s.sidebar().cloned().unwrap());
    assert_eq!(sidebar.read_with(cx, |s, _| s.selected()), Some(0));
}

#[gpui_kit::test]
fn double_clicking_a_schema_does_nothing_but_select(cx: &mut TestAppContext) {
    let (_fx, _files, win, shell) = expanded_main(cx);
    cx.update_window(win, |_, w, cx| w.double_click(("tree-row", 0usize), cx))
        .unwrap();
    cx.run_until_parked();
    assert_eq!(tree(cx, win), MAIN_OPEN);
    assert!(shell.read_with(cx, |s, _| s.tab().is_none()));
}

#[gpui_kit::test]
fn double_clicking_the_chevron_expands_once(cx: &mut TestAppContext) {
    let (_fx, _files, win, shell) = started(cx, T);
    cx.update_window(win, |_, w, cx| {
        w.double_click(part_id("connection-toggle", "lite"), cx)
    })
    .unwrap();
    wait_tree(cx, win, &MAIN_OPEN);
    assert!(shell.read_with(cx, |s, _| s.tab().is_none()));
}

#[gpui_kit::test]
fn a_single_click_on_a_connection_selects_it_without_opening_a_tab(cx: &mut TestAppContext) {
    let (_fx, _files, win, shell) = started(cx, T);
    click(cx, win, connection_id("lite"));
    assert!(shell.read_with(cx, |s, _| s.tab().is_none()));
    let sidebar = shell.read_with(cx, |s, _| s.sidebar().cloned().unwrap());
    assert_eq!(sidebar.read_with(cx, |s, _| s.selected()), Some(0));
}

#[gpui_kit::test]
fn double_clicking_a_connection_opens_its_tab(cx: &mut TestAppContext) {
    let (_fx, _files, win, shell) = started(cx, T);
    cx.update_window(win, |_, w, cx| w.double_click(connection_id("lite"), cx))
        .unwrap();
    cx.run_until_parked();
    assert_eq!(shell.read_with(cx, |s, _| s.tabs().len()), 1);
    // A second double click activates that tab instead of opening another.
    cx.update_window(win, |_, w, cx| w.double_click(connection_id("lite"), cx))
        .unwrap();
    cx.run_until_parked();
    assert_eq!(shell.read_with(cx, |s, _| s.tabs().len()), 1);
}

// The finished result's SQL and the editor text of the tab now open.
fn shown(cx: &mut TestAppContext, shell: &Entity<Shell>) -> Option<(String, String)> {
    shell.read_with(cx, |s, cx| {
        let tab = s.tab()?.read(cx);
        let r = tab.model().read(cx).results().first()?;
        r.store.end()?;
        Some((r.sql.clone(), tab.sql(cx)))
    })
}
fn wait_shown(cx: &mut TestAppContext, shell: &Entity<Shell>, sql: &str) {
    let want = Some((sql.to_owned(), sql.to_owned()));
    wait_real(cx, sql, |cx| shown(cx, shell) == want);
}

#[gpui_kit::test]
fn double_click_opens_the_select_in_a_new_tab_and_keeps_edited_sql(cx: &mut TestAppContext) {
    let (_fx, _files, win, shell) = expanded_main(cx);
    support::open_connection_row(cx, win, "lite");
    let first = shell.read_with(cx, |s, _| s.tab().cloned().unwrap());
    cx.update_window(win, |_, w, cx| {
        first.update(cx, |t, cx| t.set_sql("SELECT 42", w, cx))
    })
    .unwrap();
    cx.update_window(win, |_, w, cx| w.double_click(("tree-row", 2usize), cx))
        .unwrap();
    wait_shown(
        cx,
        &shell,
        "SELECT *\nFROM main.Album\nORDER BY AlbumId ASC",
    );
    assert!(!present(cx, win, "confirm-dialog"));
    assert_eq!(
        shell.read_with(cx, |s, _| (s.tabs().len(), s.active())),
        (2, Some(1))
    );
    assert_eq!(first.read_with(cx, |t, cx| t.sql(cx)), "SELECT 42");
}

#[gpui_kit::test]
fn double_clicking_the_same_relation_reuses_its_tab(cx: &mut TestAppContext) {
    let (_fx, _files, win, shell) = expanded_main(cx);
    cx.update_window(win, |_, w, cx| w.double_click(("tree-row", 2usize), cx))
        .unwrap();
    wait_shown(
        cx,
        &shell,
        "SELECT *\nFROM main.Album\nORDER BY AlbumId ASC",
    );
    cx.update_window(win, |_, w, cx| w.double_click(("tree-row", 2usize), cx))
        .unwrap();
    cx.run_until_parked();
    wait_shown(
        cx,
        &shell,
        "SELECT *\nFROM main.Album\nORDER BY AlbumId ASC",
    );
    assert_eq!(shell.read_with(cx, |s, _| s.tabs().len()), 1);
}

#[gpui_kit::test]
fn the_explorer_selection_follows_the_active_tab(cx: &mut TestAppContext) {
    let (_fx, _files, win, shell) = expanded_main(cx);
    support::open_connection_row(cx, win, "nopg");
    cx.update_window(win, |_, w, cx| w.double_click(("tree-row", 2usize), cx))
        .unwrap();
    wait_shown(
        cx,
        &shell,
        "SELECT *\nFROM main.Album\nORDER BY AlbumId ASC",
    );
    let sidebar = shell.read_with(cx, |s, _| s.sidebar().cloned().unwrap());
    assert_eq!(sidebar.read_with(cx, |s, _| s.selected()), Some(0));
    click(cx, win, ("tab", 0usize));
    assert_eq!(sidebar.read_with(cx, |s, _| s.selected()), Some(1));
}

// The kit has no triple click, so press `id` three times with rising click counts.
fn triple_click(cx: &mut TestAppContext, win: AnyWindowHandle, id: impl Into<ElementId>) {
    use gpui_kit::{MouseButton, MouseDownEvent, MouseUpEvent, PlatformInput};
    let id = id.into();
    cx.update_window(win, |_, w, cx| {
        w.render_frame(cx);
        let position = w.find(id).bounds().center();
        for click_count in 1..=3 {
            let down = MouseDownEvent {
                button: MouseButton::Left,
                position,
                modifiers: Default::default(),
                click_count,
                first_mouse: false,
            };
            w.dispatch_event(PlatformInput::MouseDown(down), cx);
            let up = MouseUpEvent {
                button: MouseButton::Left,
                position,
                modifiers: Default::default(),
                click_count,
            };
            w.dispatch_event(PlatformInput::MouseUp(up), cx);
            w.render_frame(cx);
        }
    })
    .unwrap();
    cx.run_until_parked();
}
// Counts the sidebar events that `matches` accepts.
fn count_events(
    cx: &mut TestAppContext,
    shell: &Entity<Shell>,
    matches: fn(&dbento_app::sidebar::SidebarEvent) -> bool,
) -> std::rc::Rc<std::cell::Cell<usize>> {
    let sidebar = shell.read_with(cx, |s, _| s.sidebar().cloned().unwrap());
    let count = std::rc::Rc::new(std::cell::Cell::new(0));
    let seen = count.clone();
    cx.update(|cx| {
        cx.subscribe(&sidebar, move |_, e, _| {
            if matches(e) {
                seen.set(seen.get() + 1);
            }
        })
        .detach()
    });
    count
}

#[gpui_kit::test]
fn a_triple_click_opens_a_relation_once(cx: &mut TestAppContext) {
    use dbento_app::sidebar::SidebarEvent;
    let (_fx, _files, win, shell) = expanded_main(cx);
    let opens = count_events(cx, &shell, |e| {
        matches!(e, SidebarEvent::OpenRelation { .. })
    });
    triple_click(cx, win, ("tree-row", 2usize));
    assert_eq!(opens.get(), 1);
}

#[gpui_kit::test]
fn a_triple_click_opens_a_connection_once(cx: &mut TestAppContext) {
    use dbento_app::sidebar::SidebarEvent;
    let (_fx, _files, win, shell) = started(cx, T);
    let opens = count_events(cx, &shell, |e| matches!(e, SidebarEvent::Open(_)));
    triple_click(cx, win, connection_id("lite"));
    assert_eq!(opens.get(), 1);
}

#[gpui_kit::test]
fn the_filter_input_is_28_px(cx: &mut TestAppContext) {
    let (_fx, _files, win, _shell) = started(cx, T);
    // A 28 px input above the panel's half-rem (6.5 px) bottom pad.
    let height = cx
        .update_window(win, |_, w, cx| {
            w.render_frame(cx);
            w.find("schema-filter").bounds().size.height
        })
        .unwrap();
    assert!(height >= gpui_kit::px(34.), "{height:?}");
}

fn lite_rel(name: &str) -> NodeKey {
    NodeKey::Relation {
        conn: "lite".into(),
        rel: RelationRef::new("main", name),
    }
}
fn selected_node(cx: &mut TestAppContext, shell: &Entity<Shell>) -> Option<NodeKey> {
    let sidebar = shell.read_with(cx, |s, _| s.sidebar().cloned().unwrap());
    sidebar.read_with(cx, |s, _| s.selected_node())
}
const ALBUM_SQL: &str = "SELECT *\nFROM main.Album\nORDER BY AlbumId ASC";
const RECENT_SQL: &str = "SELECT *\nFROM main.Recent";

#[gpui_kit::test]
fn switching_tabs_reveals_and_selects_the_tabs_table(cx: &mut TestAppContext) {
    let (_fx, _files, win, shell) = expanded_main(cx);
    cx.update_window(win, |_, w, cx| w.double_click(("tree-row", 2usize), cx))
        .unwrap();
    wait_shown(cx, &shell, ALBUM_SQL);
    cx.update_window(win, |_, w, cx| w.double_click(("tree-row", 4usize), cx))
        .unwrap();
    wait_shown(cx, &shell, RECENT_SQL);
    assert_eq!(selected_node(cx, &shell), Some(lite_rel("Recent")));
    // Collapse the connection, then activate the Album tab: its ancestors re-expand.
    click(cx, win, part_id("connection-toggle", "lite"));
    wait_tree(cx, win, &[]);
    click(cx, win, ("tab", 0usize));
    wait_tree(cx, win, &MAIN_OPEN);
    assert_eq!(selected_node(cx, &shell), Some(lite_rel("Album")));
}

#[gpui_kit::test]
fn activating_a_table_tab_loads_and_reveals_a_collapsed_tree(cx: &mut TestAppContext) {
    let (_fx, _files, win, shell) = expanded_main(cx);
    cx.update_window(win, |_, w, cx| w.double_click(("tree-row", 2usize), cx))
        .unwrap();
    wait_shown(cx, &shell, ALBUM_SQL);
    support::open_connection_row(cx, win, "nopg");
    // Fold the schema and the connection, then drop every cache: nothing is loaded.
    click(cx, win, ("tree-toggle", 0usize));
    click(cx, win, part_id("connection-toggle", "lite"));
    click(cx, win, "schema-refresh");
    let sidebar = shell.read_with(cx, |s, _| s.sidebar().cloned().unwrap());
    assert!(sidebar.read_with(cx, |s, _| s.tree().schemas("lite").is_none()));
    click(cx, win, ("tab", 0usize));
    wait_tree(cx, win, &MAIN_OPEN);
    assert_eq!(selected_node(cx, &shell), Some(lite_rel("Album")));
    assert_eq!(sidebar.read_with(cx, |s, _| s.selected()), Some(0));
}

#[gpui_kit::test]
fn a_newer_select_drops_a_pending_reveal(cx: &mut TestAppContext) {
    let (_fx, _files, win, shell) = started(cx, T);
    cx.update_window(win, |_, w, cx| {
        shell.update(cx, |s, cx| {
            s.open_relation(
                0,
                &RelationRef::new("main", "Album"),
                RelationKind::Table,
                w,
                cx,
            );
            s.sidebar()
                .cloned()
                .unwrap()
                .update(cx, |s, cx| s.select(1, cx));
        })
    })
    .unwrap();
    // The lone schema opens either way; had the reveal survived, it would select Album.
    wait_tree(cx, win, &MAIN_OPEN);
    let sidebar = shell.read_with(cx, |s, _| s.sidebar().cloned().unwrap());
    assert_eq!(sidebar.read_with(cx, |s, _| s.selected()), Some(1));
    assert_eq!(selected_node(cx, &shell), None);
}

#[gpui_kit::test]
fn revealing_a_dropped_table_keeps_its_key_selected_without_an_error(cx: &mut TestAppContext) {
    let (fx, _files, win, shell) = expanded_main(cx);
    cx.update_window(win, |_, w, cx| w.double_click(("tree-row", 2usize), cx))
        .unwrap();
    wait_shown(cx, &shell, ALBUM_SQL);
    support::open_connection_row(cx, win, "nopg");
    rusqlite::Connection::open(fx.dir.path().join("data/app.db"))
        .unwrap()
        .execute_batch("DROP VIEW Recent; DROP TABLE Album; CREATE TABLE Zed(x)")
        .unwrap();
    click(cx, win, "schema-refresh");
    let gone = ["lite.main", "lite.main/tables", "lite.main.Zed"];
    wait_tree(cx, win, &gone);
    click(cx, win, ("tab", 0usize));
    // The selection is kept by key through the refresh rebuild; no error row appears.
    assert_eq!(tree(cx, win), gone);
    assert_eq!(selected_node(cx, &shell), Some(lite_rel("Album")));
}

#[gpui_kit::test]
fn a_reveal_scrolls_its_row_into_view(cx: &mut TestAppContext) {
    let setup: String = (0..60)
        .map(|i| format!("CREATE TABLE T{i:02}(x);"))
        .collect();
    let (_fx, _files, win, shell) = started(cx, &setup);
    cx.update_window(win, |_, w, cx| {
        shell.update(cx, |s, cx| {
            s.open_relation(
                0,
                &RelationRef::new("main", "T59"),
                RelationKind::Table,
                w,
                cx,
            )
        })
    })
    .unwrap();
    // main, tables, then T00..T59: the last table is row 61, far below the first screen.
    wait_real(cx, "T59 on screen", |cx| {
        label_of(cx, win, ("tree-row", 61usize)).as_deref() == Some("lite.main.T59")
    });
    assert!(!present(cx, win, ("tree-row", 0usize)), "the list scrolled");
    assert_eq!(selected_node(cx, &shell), Some(lite_rel("T59")));
}

#[gpui_kit::test]
fn a_reveal_expands_the_folders_the_user_collapsed(cx: &mut TestAppContext) {
    init(cx);
    init_tokio(cx);
    let (fx, files) = (
        fixture(T, "folder = \"team/core\""),
        tempfile::tempdir().unwrap(),
    );
    let (win, shell) = cx
        .update(|cx| start(services(files.path()), Some(fx.dir.path().into()), cx))
        .unwrap();
    cx.update_window(win, |_, w, cx| {
        shell.update(cx, |s, cx| {
            s.open_relation(
                0,
                &RelationRef::new("main", "Album"),
                RelationKind::Table,
                w,
                cx,
            )
        })
    })
    .unwrap();
    wait_shown(cx, &shell, ALBUM_SQL);
    support::open_connection_row(cx, win, "nopg");
    click(cx, win, part_id("folder-toggle", "team/core"));
    click(cx, win, part_id("folder-toggle", "team"));
    wait_tree(cx, win, &[]);
    click(cx, win, ("tab", 0usize));
    wait_tree(cx, win, &MAIN_OPEN);
    assert_eq!(selected_node(cx, &shell), Some(lite_rel("Album")));
    let sidebar = shell.read_with(cx, |s, _| s.sidebar().cloned().unwrap());
    assert!(sidebar.read_with(cx, |s, _| s.collapsed_paths().is_empty()));
}

#[gpui_kit::test]
fn collapsing_during_a_reveal_stops_it(cx: &mut TestAppContext) {
    let (_fx, _files, win, shell) = started(cx, T);
    let sidebar = shell.read_with(cx, |s, _| s.sidebar().cloned().unwrap());
    cx.update_window(win, |_, w, cx| {
        shell.update(cx, |s, cx| {
            s.open_relation(
                0,
                &RelationRef::new("main", "Album"),
                RelationKind::Table,
                w,
                cx,
            )
        });
        sidebar.update(cx, |s, cx| {
            s.toggle_node(NodeKey::Connection("lite".into()), cx)
        });
    })
    .unwrap();
    wait_real(cx, "the schemas", |cx| {
        sidebar.read_with(cx, |s, _| s.tree().schemas("lite").is_some())
    });
    cx.run_until_parked();
    let conn = NodeKey::Connection("lite".into());
    assert!(!sidebar.read_with(cx, |s, _| s.tree().is_expanded(&conn)));
    assert_eq!(tree(cx, win), Vec::<String>::new());
}

const OBJ: &str = "CREATE TABLE Album(AlbumId INTEGER PRIMARY KEY, Title TEXT NOT NULL); \
                   CREATE INDEX album_title ON Album(Title); \
                   CREATE VIEW Recent AS SELECT Title FROM Album; \
                   CREATE TRIGGER recent_add INSTEAD OF INSERT ON Recent BEGIN SELECT 1; END;";
const ALBUM_GROUPS: [&str; 9] = [
    "lite.main",
    "lite.main/tables",
    "lite.main.Album",
    "lite.main.Album.AlbumId",
    "lite.main.Album.Title",
    "lite.main.Album/keys",
    "lite.main.Album/indexes",
    "lite.main/views",
    "lite.main.Recent",
];
const INDEX_ROW: &str = "lite.main.Album/indexes.album_title";

/// `OBJ` with lite, main and Album open and Album's indexes group expanded (index at row 7).
fn index_open(cx: &mut TestAppContext) -> (Fx, tempfile::TempDir, AnyWindowHandle, Entity<Shell>) {
    let (fx, files, win, shell) = started(cx, OBJ);
    click(cx, win, part_id("connection-toggle", "lite"));
    wait_tree(cx, win, &MAIN_OPEN);
    click(cx, win, ("tree-toggle", 2usize));
    wait_tree(cx, win, &ALBUM_GROUPS);
    click(cx, win, ("tree-toggle", 6usize));
    let mut want = ALBUM_GROUPS.to_vec();
    want.insert(7, INDEX_ROW);
    wait_tree(cx, win, &want);
    (fx, files, win, shell)
}
fn index_ref(cx: &mut TestAppContext, shell: &Entity<Shell>) -> ObjectRef {
    let sidebar = shell.read_with(cx, |s, _| s.sidebar().cloned().unwrap());
    sidebar
        .read_with(cx, |s, _| {
            s.tree()
                .rows("lite", 1, "")
                .into_iter()
                .find_map(|r| match r.kind {
                    RowKind::Object { object, .. } if r.aria == INDEX_ROW => Some(object),
                    _ => None,
                })
        })
        .expect("the index row")
}
fn press_key(cx: &mut TestAppContext, win: AnyWindowHandle, key: &str) {
    cx.update_window(win, |_, w, cx| w.press(key, cx)).unwrap();
    cx.run_until_parked();
}
// The ViewDefinition events the sidebar emits, as (connection, object).
fn view_requests(
    cx: &mut TestAppContext,
    shell: &Entity<Shell>,
) -> Rc<RefCell<Vec<(String, ObjectRef)>>> {
    let sidebar = shell.read_with(cx, |s, _| s.sidebar().cloned().unwrap());
    let seen = Rc::new(RefCell::new(Vec::new()));
    let out = seen.clone();
    cx.update(|cx| {
        cx.subscribe(&sidebar, move |_, e: &SidebarEvent, _| {
            if let SidebarEvent::ViewDefinition { connection, object } = e {
                out.borrow_mut().push((connection.clone(), object.clone()));
            }
        })
        .detach()
    });
    seen
}

#[gpui_kit::test]
fn a_tables_objects_sit_in_collapsed_groups_after_its_columns(cx: &mut TestAppContext) {
    let (_fx, _files, win, shell) = index_open(cx);
    let sidebar = shell.read_with(cx, |s, _| s.sidebar().cloned().unwrap());
    let index = sidebar.read_with(cx, |s, _| {
        s.tree()
            .rows("lite", 1, "")
            .into_iter()
            .find(|r| r.aria == INDEX_ROW)
            .unwrap()
    });
    assert_eq!(index.label, "album_title");
    assert!(
        matches!(&index.kind, RowKind::Object { group: ObjectGroup::Indexes, detail, .. } if detail.is_empty()),
        "{:?}",
        index.kind
    );
    // A view's INSTEAD OF trigger is listed under the view.
    click(cx, win, ("tree-toggle", 9usize));
    wait_real(cx, "Recent's triggers", |cx| {
        tree(cx, win)
            .iter()
            .any(|r| r == "lite.main.Recent/triggers")
    });
}

#[gpui_kit::test]
fn enter_or_a_double_click_on_an_object_asks_for_its_definition(cx: &mut TestAppContext) {
    let (_fx, _files, win, shell) = index_open(cx);
    let seen = view_requests(cx, &shell);
    // Selecting the group and stepping onto the index fires nothing.
    click(cx, win, ("tree-row", 6usize));
    press_key(cx, win, "down");
    assert!(seen.borrow().is_empty(), "browsing asks for no definition");
    let sidebar = shell.read_with(cx, |s, _| s.sidebar().cloned().unwrap());
    let (conn, object) = sidebar
        .read_with(cx, |s, _| s.selected_object())
        .expect("the index is selected");
    assert_eq!(conn, "lite");
    assert_eq!(object, index_ref(cx, &shell));
    press_key(cx, win, "enter");
    assert_eq!(*seen.borrow(), [("lite".to_owned(), object.clone())]);
    cx.update_window(win, |_, w, cx| w.double_click(("tree-row", 7usize), cx))
        .unwrap();
    cx.run_until_parked();
    assert_eq!(seen.borrow().len(), 2);
    assert_eq!(seen.borrow()[1], ("lite".to_owned(), object));
}

#[gpui_kit::test]
fn a_definition_reply_leaves_the_sidebar_as_an_event(cx: &mut TestAppContext) {
    let (_fx, _files, win, shell) = index_open(cx);
    let object = index_ref(cx, &shell);
    let sidebar = shell.read_with(cx, |s, _| s.sidebar().cloned().unwrap());
    let got: Rc<RefCell<Vec<Result<Definition, String>>>> = Rc::default();
    let out = got.clone();
    let want = object.clone();
    cx.update(|cx| {
        cx.subscribe(&sidebar, move |_, e: &SidebarEvent, _| {
            if let SidebarEvent::Definition {
                connection,
                object,
                result,
            } = e
            {
                assert_eq!((connection.as_str(), object), ("lite", &want));
                out.borrow_mut().push(result.clone());
            }
        })
        .detach()
    });
    sidebar.update(cx, |s, cx| s.load_definition("lite", &object, cx));
    wait_real(cx, "the definition", |_| !got.borrow().is_empty());
    let text = got.borrow()[0].clone().map(|d| d.text().to_owned());
    assert_eq!(
        text,
        Ok("CREATE INDEX album_title ON Album(Title);".to_owned())
    );
    assert_eq!(tree(cx, win).len(), 10, "a definition is no tree level");
}

#[gpui_kit::test]
fn the_object_menu_asks_for_that_objects_definition(cx: &mut TestAppContext) {
    let (_fx, _files, win, shell) = index_open(cx);
    let seen = view_requests(cx, &shell);
    cx.update_window(win, |_, w, cx| w.right_click(("tree-row", 7usize), cx))
        .unwrap();
    cx.run_until_parked();
    assert!(present(cx, win, "tree-menu-copy-name"));
    click(cx, win, "tree-menu-view-definition");
    let index = index_ref(cx, &shell);
    assert_eq!(*seen.borrow(), [("lite".to_owned(), index)]);
}

#[gpui_kit::test]
fn retry_on_a_failed_objects_row_reloads_them(cx: &mut TestAppContext) {
    let (_fx, _files, win, shell) = index_open(cx);
    let sidebar = shell.read_with(cx, |s, _| s.sidebar().cloned().unwrap());
    let generation = sidebar.read_with(cx, |s, _| s.tree().generation());
    let failed = CatalogReply {
        generation,
        query: CatalogQuery::RelationObjects {
            rel: RelationRef::new("main", "Album"),
        },
        result: Err("permission denied".into()),
    };
    sidebar.update(cx, |s, cx| s.apply_replies("lite", vec![failed], cx));
    cx.run_until_parked();
    let rows = tree(cx, win);
    let ix = rows
        .iter()
        .position(|r| r == "lite.main.Album/objects:error")
        .unwrap_or_else(|| panic!("no objects error row in {rows:?}"));
    assert!(!rows.iter().any(|r| r == "lite.main.Album/keys"));
    click(cx, win, ("tree-retry", ix));
    let mut want = ALBUM_GROUPS.to_vec();
    want.insert(7, INDEX_ROW);
    wait_tree(cx, win, &want);
}

#[gpui_kit::test]
fn a_selected_catalog_node_survives_a_filter_that_hides_it(cx: &mut TestAppContext) {
    let (_fx, _files, win, shell) = expanded_main(cx);
    click(cx, win, ("tree-row", 2usize));
    assert_eq!(selected_node(cx, &shell), Some(lite_rel("Album")));
    let filter = shell.read_with(cx, |s, cx| {
        s.sidebar().unwrap().read(cx).filter_state().clone()
    });
    cx.update_window(win, |_, w, cx| {
        filter.update(cx, |f, cx| f.focus(w, cx));
        w.input("rec", cx);
    })
    .unwrap();
    wait_tree(
        cx,
        win,
        &["lite.main", "lite.main/views", "lite.main.Recent"],
    );
    assert_eq!(selected_node(cx, &shell), Some(lite_rel("Album")));
    cx.update_window(win, |_, w, cx| {
        for _ in 0..3 {
            w.press("backspace", cx);
        }
    })
    .unwrap();
    wait_tree(cx, win, &MAIN_OPEN);
    assert_eq!(selected_node(cx, &shell), Some(lite_rel("Album")));
}

#[gpui_kit::test]
fn a_reveal_after_the_catalogs_closed_leaves_nothing_loading(cx: &mut TestAppContext) {
    let (_fx, _files, _win, shell) = started(cx, T);
    let sidebar = shell.read_with(cx, |s, _| s.sidebar().cloned().unwrap());
    drop(sidebar.update(cx, |s, _| s.close_catalogs()));
    sidebar.update(cx, |s, cx| s.reveal(lite_rel("Album"), cx));
    cx.run_until_parked();
    let link = sidebar.read_with(cx, |s, _| s.tree().link("lite"));
    assert_eq!(
        link,
        dbento_app::link::LinkState::Idle,
        "a level waits on no runner"
    );
    assert_eq!(selected_node(cx, &shell), Some(lite_rel("Album")));
}

#[gpui_kit::test]
fn an_object_row_is_a_leaf_to_right_and_left_steps_to_its_group(cx: &mut TestAppContext) {
    let (_fx, _files, win, shell) = index_open(cx);
    let sidebar = shell.read_with(cx, |s, _| s.sidebar().cloned().unwrap());
    let open = tree(cx, win);
    click(cx, win, ("tree-row", 7usize));
    let index = index_ref(cx, &shell);
    press_key(cx, win, "right");
    assert_eq!(tree(cx, win), open, "Right opens nothing");
    assert_eq!(
        sidebar.read_with(cx, |s, _| s.selected_object()),
        Some(("lite".to_owned(), index)),
        "Right keeps the object selected"
    );
    press_key(cx, win, "left");
    assert_eq!(tree(cx, win), open, "Left closes nothing");
    let group = NodeKey::RelationGroup {
        conn: "lite".into(),
        rel: RelationRef::new("main", "Album"),
        group: ObjectGroup::Indexes,
    };
    assert_eq!(sidebar.read_with(cx, |s, _| s.selected_node()), Some(group));
    assert!(sidebar.read_with(cx, |s, _| s.selected_object()).is_none());
}
