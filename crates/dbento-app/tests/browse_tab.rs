//! Opening a table from the explorer makes a browse tab that runs exactly its browse SQL.
use crate::support;
use crate::support::{Opened, label_of, launched, wait_real};
use dbento_app::explorer::part_id;
use dbento_app::schema_tree::select_sql;
use dbento_core::{Engine, RelationKind, RelationRef};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext};

const T: &str = "CREATE TABLE Album(AlbumId INTEGER PRIMARY KEY, Title TEXT NOT NULL); \
                 CREATE TABLE plain(a, b); \
                 CREATE VIEW Recent AS SELECT Title FROM Album; \
                 INSERT INTO Album(Title) VALUES ('a'), ('b'), ('c'); \
                 INSERT INTO plain VALUES (1, 'x');";
const ALBUM: &str = "SELECT *\nFROM main.Album\nORDER BY AlbumId ASC";
const PLAIN: &str = "SELECT rowid, *\nFROM main.plain\nORDER BY rowid ASC";

fn tabs(cx: &mut TestAppContext, o: &Opened) -> usize {
    o.shell.read_with(cx, |s, _| s.tabs().len())
}
fn browsing(cx: &mut TestAppContext, o: &Opened) -> bool {
    o.shell.read_with(cx, |s, cx| {
        s.tab()
            .unwrap()
            .read(cx)
            .model()
            .read(cx)
            .browse()
            .is_some()
    })
}

#[gpui_kit::test]
fn a_table_opens_as_a_browse_tab_running_exactly_its_browse_sql(cx: &mut TestAppContext) {
    let o = launched(cx, T, "");
    o.open_relation(cx, "plain", RelationKind::Table);
    o.wait_browse(cx, PLAIN);
    let edited = o
        .shell
        .read_with(cx, |s, cx| s.tab().unwrap().read(cx).edited(cx));
    assert!(!edited, "generated SQL counts as saved");
}

#[gpui_kit::test]
fn a_pk_table_shows_the_text_select_sql_gives(cx: &mut TestAppContext) {
    let o = launched(cx, T, "");
    o.open_relation(cx, "Album", RelationKind::Table);
    o.wait_browse(cx, ALBUM);
    // The browse is the plain SELECT plus the key order that keeps pages stable.
    assert_eq!(
        format!(
            "{}\nORDER BY AlbumId ASC",
            select_sql(Engine::Sqlite, &RelationRef::new("main", "Album"))
        ),
        ALBUM
    );
}

#[gpui_kit::test]
fn a_view_opens_as_a_plain_tab(cx: &mut TestAppContext) {
    let o = launched(cx, T, "");
    o.open_relation(cx, "Recent", RelationKind::View);
    let want = "SELECT *\nFROM main.Recent".to_owned();
    wait_real(cx, "the view", |cx| {
        o.browse_shown(cx) == Some((want.clone(), want.clone(), None))
    });
}

#[gpui_kit::test]
fn a_table_that_cannot_browse_says_why_it_runs_as_a_plain_query(cx: &mut TestAppContext) {
    // A table the explorer still lists but the database no longer has cannot be described.
    let o = launched(cx, T, "");
    o.open_relation(cx, "gone", RelationKind::Table);
    let want = "Opened as a plain query, so rows cannot be edited here: \
                no table or view main.gone";
    wait_real(cx, "the notice", |cx| {
        label_of(cx, o.win, "notice").as_deref() == Some(want)
    });
    assert!(!browsing(cx, &o));
}

#[gpui_kit::test]
fn typing_in_the_editor_clears_the_browse_state(cx: &mut TestAppContext) {
    let o = launched(cx, T, "");
    o.open_relation(cx, "Album", RelationKind::Table);
    o.wait_browse(cx, ALBUM);
    let e = o
        .shell
        .read_with(cx, |s, cx| s.tab().unwrap().read(cx).editor().clone());
    cx.update_window(o.win, |_, w, cx| {
        e.update(cx, |e, cx| e.focus(w, cx));
        w.input(" ", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert!(!browsing(cx, &o));
    // The edited tab is not reused: the same table opens a second, browsing tab.
    o.open_relation(cx, "Album", RelationKind::Table);
    assert_eq!(tabs(cx, &o), 2);
    o.wait_browse(cx, ALBUM);
}

#[gpui_kit::test]
fn opening_the_same_table_again_reuses_its_tab_and_reruns(cx: &mut TestAppContext) {
    let o = launched(cx, T, "");
    o.open_relation(cx, "plain", RelationKind::Table);
    o.wait_browse(cx, PLAIN);
    let model = o
        .shell
        .read_with(cx, |s, cx| s.tab().unwrap().read(cx).model().clone());
    let before = model.read_with(cx, |m, _| m.run_id());
    o.open_relation(cx, "plain", RelationKind::Table);
    assert_eq!(tabs(cx, &o), 1);
    wait_real(cx, "the rerun", |cx| {
        model.read_with(cx, |m, _| m.run_id() > before && !m.running())
    });
    o.wait_browse(cx, PLAIN);
}

#[gpui_kit::test]
fn a_filter_rewrites_the_editor_and_runs(cx: &mut TestAppContext) {
    let o = launched(cx, T, "");
    o.open_relation(cx, "Album", RelationKind::Table);
    o.wait_browse(cx, ALBUM);
    let model = o
        .shell
        .read_with(cx, |s, cx| s.tab().unwrap().read(cx).model().clone());
    assert!(model.update(cx, |m, cx| m.apply_filter("AlbumId > 1", cx)));
    o.wait_browse(
        cx,
        "SELECT *\nFROM main.Album\nWHERE (\nAlbumId > 1\n)\nORDER BY AlbumId ASC",
    );
    assert!(
        browsing(cx, &o),
        "writing the browse SQL must not clear the state"
    );
    assert_eq!(
        label_of(cx, o.win, "footer-rows").as_deref(),
        Some("2 rows")
    );
}

#[gpui_kit::test]
fn the_explorer_double_click_opens_tables_as_browse_tabs_and_views_plainly(
    cx: &mut TestAppContext,
) {
    let o = launched(cx, T, "");
    let tree = |cx: &mut TestAppContext| -> Vec<String> {
        (0usize..)
            .map_while(|i| label_of(cx, o.win, ("tree-row", i)))
            .collect()
    };
    cx.update_window(o.win, |_, w, cx| {
        w.click(part_id("connection-toggle", "lite"), cx)
    })
    .unwrap();
    let open = [
        "lite.main",
        "lite.main/tables",
        "lite.main.Album",
        "lite.main.plain",
        "lite.main/views",
        "lite.main.Recent",
    ];
    wait_real(cx, "the relations", |cx| tree(cx) == open);
    cx.update_window(o.win, |_, w, cx| w.double_click(("tree-row", 5usize), cx))
        .unwrap();
    let recent = "SELECT *\nFROM main.Recent".to_owned();
    wait_real(cx, "the view", |cx| {
        o.browse_shown(cx) == Some((recent.clone(), recent.clone(), None))
    });
    cx.update_window(o.win, |_, w, cx| w.double_click(("tree-row", 3usize), cx))
        .unwrap();
    o.wait_browse(cx, PLAIN);
}

#[gpui_kit::test]
fn a_table_that_cannot_be_described_reuses_its_tab(cx: &mut TestAppContext) {
    let o = launched(cx, T, "");
    let before = tabs(cx, &o);
    o.open_relation(cx, "gone", RelationKind::Table);
    wait_real(cx, "the fallback run", |cx| o.browse_shown(cx).is_some());
    assert_eq!(tabs(cx, &o), before + 1);
    o.open_relation(cx, "gone", RelationKind::Table);
    assert_eq!(
        tabs(cx, &o),
        before + 1,
        "a second double click reuses the tab"
    );
}

#[gpui_kit::test]
fn a_saved_query_holding_a_failed_tables_select_is_not_reused(cx: &mut TestAppContext) {
    let o = launched(cx, T, "");
    let before = tabs(cx, &o);
    o.open_relation(cx, "gone", RelationKind::Table);
    wait_real(cx, "the fallback run", |cx| o.browse_shown(cx).is_some());
    let tab = o.shell.read_with(cx, |s, _| s.tab().unwrap().clone());
    tab.update(cx, |t, cx| {
        let sql = t.sql(cx);
        t.set_file("queries/gone.sql".into(), sql, cx);
    });
    o.open_relation(cx, "gone", RelationKind::Table);
    assert_eq!(tabs(cx, &o), before + 2, "a saved query keeps its file");
}

#[gpui_kit::test]
fn a_typed_tab_holding_a_tables_select_is_not_reused(cx: &mut TestAppContext) {
    let o = support::opened(cx, T, "", 0);
    let select = select_sql(Engine::Sqlite, &RelationRef::new("main", "gone"));
    o.run(cx, &select);
    o.idle(cx);
    let before = tabs(cx, &o);
    o.open_relation(cx, "gone", RelationKind::Table);
    assert_eq!(
        tabs(cx, &o),
        before + 1,
        "a tab the user typed is never taken over"
    );
}
