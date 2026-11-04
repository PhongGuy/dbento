//! The definition dock in the app: opened from the explorer, one dock at a time, Reload
//! and Open in query tab.
mod support;
use dbento_app::explorer::part_id;
use dbento_core::{ObjectRef, RelationRef};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext};
use support::{Opened, click_id, label_of, launched, open_connection_row, present, wait_real};

const OBJ: &str = "CREATE TABLE Album(AlbumId INTEGER PRIMARY KEY, Title TEXT NOT NULL); \
                   CREATE INDEX album_title ON Album(Title);";
const INDEX_SQL: &str = "CREATE INDEX album_title ON Album(Title);";
const TABLE_SQL: &str = "CREATE TABLE Album(AlbumId INTEGER PRIMARY KEY, Title TEXT NOT NULL);";

/// Opens the dock on `conn`'s Album and waits for its text.
fn view_album(cx: &mut TestAppContext, o: &Opened, conn: &str) {
    let object = ObjectRef::relation(RelationRef::new("main", "Album"));
    let shell = o.shell.clone();
    cx.update_window(o.win, |_, w, cx| {
        shell.update(cx, |s, cx| s.view_definition(conn, object, w, cx))
    })
    .unwrap();
    wait_real(cx, "the table's definition", |cx| {
        label_of(cx, o.win, "definition-editor").as_deref() == Some(TABLE_SQL)
    });
}

#[gpui_kit::test]
fn enter_on_an_index_opens_its_definition_in_the_dock(cx: &mut TestAppContext) {
    let o = launched(cx, OBJ, "");
    let tree = |cx: &mut TestAppContext| -> Vec<String> {
        (0usize..)
            .map_while(|i| label_of(cx, o.win, ("tree-row", i)))
            .collect()
    };
    click_id(cx, o.win, part_id("connection-toggle", "lite"));
    wait_real(cx, "Album", |cx| tree(cx).len() == 3);
    click_id(cx, o.win, ("tree-toggle", 2usize));
    wait_real(cx, "Album's groups", |cx| {
        tree(cx).get(6).map(String::as_str) == Some("lite.main.Album/indexes")
    });
    click_id(cx, o.win, ("tree-toggle", 6usize));
    wait_real(cx, "the index row", |cx| tree(cx).len() == 8);
    click_id(cx, o.win, ("tree-row", 7usize));
    cx.update_window(o.win, |_, w, cx| w.press("enter", cx))
        .unwrap();
    wait_real(cx, "the definition", |cx| {
        label_of(cx, o.win, "definition-editor").as_deref() == Some(INDEX_SQL)
    });
    assert!(o.shell.read_with(cx, |s, _| s.definition_open()
        && !s.inspector_open()
        && !s.history_open()));
    assert_eq!(
        label_of(cx, o.win, "definition-title").as_deref(),
        Some("Index album_title on main.Album")
    );
    // Reload fetches fresh: a dropped index now says so.
    rusqlite::Connection::open(o.fx.dir.path().join("data/app.db"))
        .unwrap()
        .execute_batch("DROP INDEX album_title")
        .unwrap();
    click_id(cx, o.win, "definition-reload");
    wait_real(cx, "the missing note", |cx| {
        label_of(cx, o.win, "definition-error").is_some_and(|e| e.contains("no longer exists"))
    });
}

#[gpui_kit::test]
fn open_in_tab_copies_the_text_unrun_and_one_dock_shows_at_a_time(cx: &mut TestAppContext) {
    let o = launched(cx, OBJ, "");
    let object = ObjectRef::relation(RelationRef::new("main", "Album"));
    let shell = o.shell.clone();
    cx.update_window(o.win, |_, w, cx| {
        shell.update(cx, |s, cx| s.view_definition("lite", object, w, cx))
    })
    .unwrap();
    let table = "CREATE TABLE Album(AlbumId INTEGER PRIMARY KEY, Title TEXT NOT NULL);";
    wait_real(cx, "the table's definition", |cx| {
        label_of(cx, o.win, "definition-editor").as_deref() == Some(table)
    });
    click_id(cx, o.win, "definition-open-tab");
    let shown = o.shell.read_with(cx, |s, cx| {
        s.tab().map(|t| {
            let tab = t.read(cx);
            (tab.sql(cx), tab.model().read(cx).results().is_empty())
        })
    });
    assert_eq!(
        shown,
        Some((table.to_owned(), true)),
        "the tab holds the text, unrun"
    );
    click_id(cx, o.win, "history-toggle");
    assert!(
        o.shell
            .read_with(cx, |s, _| s.history_open() && !s.definition_open())
    );
    assert!(!present(cx, o.win, "definition-panel"));
}

#[gpui_kit::test]
fn changing_the_workspace_closes_the_definition_dock(cx: &mut TestAppContext) {
    let o = launched(cx, OBJ, "");
    let shell = o.shell.clone();
    let table = "CREATE TABLE Album(AlbumId INTEGER PRIMARY KEY, Title TEXT NOT NULL);";
    let view = |cx: &mut TestAppContext| {
        let object = ObjectRef::relation(RelationRef::new("main", "Album"));
        cx.update_window(o.win, |_, w, cx| {
            shell.update(cx, |s, cx| s.view_definition("lite", object, w, cx))
        })
        .unwrap();
        wait_real(cx, "the table's definition", |cx| {
            label_of(cx, o.win, "definition-editor").as_deref() == Some(table)
        });
    };
    let switch = |cx: &mut TestAppContext, dir: std::path::PathBuf| {
        cx.update_window(o.win, |_, w, cx| {
            shell.update(cx, |s, cx| s.open_workspace(dir, w, cx))
        })
        .unwrap();
        // Closed and emptied, so no object of the old workspace survives a reopen.
        let left = shell.read_with(cx, |s, cx| {
            (
                s.definition_open(),
                s.definition_panel().read(cx).text().is_some(),
            )
        });
        assert_eq!(left, (false, false));
        assert!(!present(cx, o.win, "definition-panel"));
    };
    view(cx);
    let other = support::fixture(OBJ, "");
    switch(cx, other.dir.path().into());
    view(cx);
    let not_a_workspace = tempfile::tempdir().unwrap();
    switch(cx, not_a_workspace.path().into());
}

#[gpui_kit::test]
fn the_close_button_closes_the_dock(cx: &mut TestAppContext) {
    let o = launched(cx, OBJ, "");
    view_album(cx, &o, "lite");
    click_id(cx, o.win, "definition-close");
    assert!(!o.shell.read_with(cx, |s, _| s.definition_open()));
    assert!(!present(cx, o.win, "definition-panel"));
}

#[gpui_kit::test]
fn the_approvals_panel_and_the_definition_dock_take_turns(cx: &mut TestAppContext) {
    let o = launched(cx, OBJ, "");
    let docks = |cx: &mut TestAppContext| {
        o.shell
            .read_with(cx, |s, _| (s.approvals_open(), s.definition_open()))
    };
    view_album(cx, &o, "lite");
    o.shell.update(cx, |s, cx| s.toggle_approvals(cx));
    cx.run_until_parked();
    assert_eq!(docks(cx), (true, false), "approvals replace the definition");
    assert!(!present(cx, o.win, "definition-panel"));
    view_album(cx, &o, "lite");
    assert_eq!(docks(cx), (false, true), "a definition replaces approvals");
}

#[gpui_kit::test]
fn open_in_tab_uses_the_definitions_connection(cx: &mut TestAppContext) {
    // A second connection on the same file, listed after the active `lite`.
    let two = "[connections.two]\nengine = \"sqlite\"\npath = \"data/app.db\"";
    let o = launched(cx, OBJ, two);
    open_connection_row(cx, o.win, "lite");
    let connection = |cx: &mut TestAppContext| {
        o.shell.read_with(cx, |s, cx| {
            s.tab()
                .map(|t| t.read(cx).model().read(cx).spec().connection.clone())
        })
    };
    assert_eq!(connection(cx).as_deref(), Some("lite"));
    view_album(cx, &o, "two");
    click_id(cx, o.win, "definition-open-tab");
    assert_eq!(connection(cx).as_deref(), Some("two"));
    let sql = o
        .shell
        .read_with(cx, |s, cx| s.tab().map(|t| t.read(cx).sql(cx)));
    assert_eq!(sql.as_deref(), Some(TABLE_SQL));
}
