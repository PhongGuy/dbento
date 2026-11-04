//! The explorer tree under the keyboard: Up and Down, Right and Left, Home and End, the row
//! kept in view, and nothing happening while the explorer lacks focus.
mod support;
use dbento_app::explorer::{connection_id, folder_id};
use dbento_app::schema_tree::NodeKey;
use dbento_core::FolderPath;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AnyWindowHandle, AppContext as _, TestAppContext};
use support::{Opened, click_id, label_of, launched, present, wait_real};

const T: &str = "CREATE TABLE Album(AlbumId INTEGER PRIMARY KEY, Title TEXT NOT NULL); \
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
fn press(cx: &mut TestAppContext, o: &Opened, keys: &str) {
    cx.update_window(o.win, |_, w, cx| w.press(keys, cx))
        .unwrap();
    cx.run_until_parked();
}

/// The selection as one word: `folder:<path>`, `conn:<name>`, or the node's short name.
fn sel(cx: &mut TestAppContext, o: &Opened) -> String {
    let sidebar = o.shell.read_with(cx, |s, _| s.sidebar().cloned().unwrap());
    sidebar.read_with(cx, |s, _| {
        if let Some(path) = s.selected_folder() {
            return format!("folder:{}", path.as_str());
        }
        match s.selected_node() {
            Some(NodeKey::Schema { schema, .. }) => format!("schema:{schema}"),
            Some(NodeKey::Kind { kind, .. }) => format!("kind:{kind:?}"),
            Some(NodeKey::Relation { rel, .. }) => format!("rel:{}", rel.name()),
            Some(other) => format!("{other:?}"),
            None => match s.selected() {
                Some(ix) => format!("conn:{ix}"),
                None => "none".to_owned(),
            },
        }
    })
}

/// The app with `lite` selected and focused by a click, then expanded by Right.
fn expanded(cx: &mut TestAppContext, setup: &str) -> Opened {
    let o = launched(cx, setup, "");
    click_id(cx, o.win, connection_id("lite"));
    press(cx, &o, "right");
    wait_tree(cx, o.win, &MAIN_OPEN);
    assert_eq!(sel(cx, &o), "conn:0", "Right expands in place");
    o
}

#[gpui_kit::test]
fn down_and_up_walk_the_visible_rows_and_stop_at_the_ends(cx: &mut TestAppContext) {
    let o = expanded(cx, T);
    let walk = [
        "schema:main",
        "kind:Table",
        "rel:Album",
        "kind:View",
        "rel:Recent",
        "conn:1",
    ];
    for want in walk {
        press(cx, &o, "down");
        assert_eq!(sel(cx, &o), want);
    }
    press(cx, &o, "down");
    assert_eq!(sel(cx, &o), "conn:1", "Down stops at the last row");
    for want in walk.iter().rev().skip(1).chain(["conn:0"].iter()) {
        press(cx, &o, "up");
        assert_eq!(sel(cx, &o), *want);
    }
    press(cx, &o, "up");
    assert_eq!(sel(cx, &o), "conn:0", "Up stops at the first row");
}

#[gpui_kit::test]
fn right_expands_then_enters_the_first_child(cx: &mut TestAppContext) {
    let o = expanded(cx, T);
    press(cx, &o, "right");
    assert_eq!(sel(cx, &o), "schema:main");
    press(cx, &o, "right");
    assert_eq!(sel(cx, &o), "kind:Table");
    press(cx, &o, "right");
    assert_eq!(sel(cx, &o), "rel:Album");
    // A collapsed relation loads its columns lazily, and keeps the selection.
    press(cx, &o, "right");
    wait_real(cx, "Album's columns", |cx| {
        tree(cx, o.win)
            .iter()
            .any(|r| r == "lite.main.Album.AlbumId")
    });
    assert_eq!(sel(cx, &o), "rel:Album");
    // Columns are not selectable, so an open relation has no child to enter.
    press(cx, &o, "right");
    assert_eq!(sel(cx, &o), "rel:Album");
}

#[gpui_kit::test]
fn left_collapses_then_goes_to_the_parent(cx: &mut TestAppContext) {
    let o = expanded(cx, T);
    click_id(cx, o.win, ("tree-row", 2usize));
    assert_eq!(sel(cx, &o), "rel:Album");
    press(cx, &o, "left");
    assert_eq!(
        sel(cx, &o),
        "kind:Table",
        "a collapsed row goes to its parent"
    );
    press(cx, &o, "left");
    wait_tree(
        cx,
        o.win,
        &[
            "lite.main",
            "lite.main/tables",
            "lite.main/views",
            "lite.main.Recent",
        ],
    );
    assert_eq!(
        sel(cx, &o),
        "kind:Table",
        "an expanded row collapses in place"
    );
    press(cx, &o, "left");
    assert_eq!(sel(cx, &o), "schema:main");
    press(cx, &o, "left");
    wait_tree(cx, o.win, &["lite.main"]);
    press(cx, &o, "left");
    assert_eq!(sel(cx, &o), "conn:0");
    press(cx, &o, "left");
    wait_tree(cx, o.win, &[]);
    press(cx, &o, "left");
    assert_eq!(sel(cx, &o), "conn:0", "a top-level row has no parent");
}

#[gpui_kit::test]
fn left_and_right_walk_folders(cx: &mut TestAppContext) {
    let o = launched(cx, T, "folder = \"team\"");
    let team = "team".parse::<FolderPath>().unwrap();
    // A folder click focuses the explorer too, so the arrows work straight after it.
    click_id(cx, o.win, folder_id(&team));
    assert_eq!(sel(cx, &o), "folder:team");
    press(cx, &o, "down");
    assert_eq!(sel(cx, &o), "conn:0");
    press(cx, &o, "left");
    assert_eq!(sel(cx, &o), "folder:team");
    press(cx, &o, "left");
    assert!(!present(cx, o.win, connection_id("lite")), "team collapsed");
    press(cx, &o, "down");
    assert_eq!(sel(cx, &o), "conn:1", "Down skips the hidden row");
    press(cx, &o, "up");
    press(cx, &o, "right");
    assert!(present(cx, o.win, connection_id("lite")), "team expanded");
    assert_eq!(sel(cx, &o), "folder:team");
    press(cx, &o, "right");
    assert_eq!(sel(cx, &o), "conn:0");
}

#[gpui_kit::test]
fn home_and_end_go_to_the_first_and_last_rows(cx: &mut TestAppContext) {
    let o = expanded(cx, T);
    press(cx, &o, "end");
    assert_eq!(sel(cx, &o), "conn:1");
    press(cx, &o, "home");
    assert_eq!(sel(cx, &o), "conn:0");
}

#[gpui_kit::test]
fn a_filtered_tree_walks_only_the_matching_rows(cx: &mut TestAppContext) {
    let o = expanded(cx, T);
    let filter = o.shell.read_with(cx, |s, cx| {
        s.sidebar().unwrap().read(cx).filter_state().clone()
    });
    cx.update_window(o.win, |_, w, cx| {
        filter.update(cx, |f, cx| f.focus(w, cx));
        w.input("rec", cx);
    })
    .unwrap();
    wait_tree(
        cx,
        o.win,
        &["lite.main", "lite.main/views", "lite.main.Recent"],
    );
    click_id(cx, o.win, connection_id("lite"));
    for want in ["schema:main", "kind:View", "rel:Recent", "conn:1"] {
        press(cx, &o, "down");
        assert_eq!(sel(cx, &o), want);
    }
}

#[gpui_kit::test]
fn a_moved_selection_scrolls_into_view(cx: &mut TestAppContext) {
    let setup: String = (0..60)
        .map(|i| format!("CREATE TABLE T{i:02}(x);"))
        .collect();
    let o = launched(cx, &setup, "");
    click_id(cx, o.win, connection_id("lite"));
    press(cx, &o, "right");
    wait_real(cx, "the tables", |cx| {
        label_of(cx, o.win, ("tree-row", 2usize)).as_deref() == Some("lite.main.T00")
    });
    // End lands on `nopg`, below main, tables and T00..T59.
    press(cx, &o, "end");
    assert_eq!(sel(cx, &o), "conn:1");
    assert!(present(cx, o.win, connection_id("nopg")));
    assert!(
        !present(cx, o.win, connection_id("lite")),
        "the list scrolled"
    );
    press(cx, &o, "up");
    assert_eq!(sel(cx, &o), "rel:T59");
    assert!(present(cx, o.win, ("tree-row", 61usize)));
    press(cx, &o, "home");
    assert!(present(cx, o.win, connection_id("lite")), "back at the top");
}

#[gpui_kit::test]
fn the_keys_do_nothing_while_the_explorer_lacks_focus(cx: &mut TestAppContext) {
    let o = launched(cx, T, "");
    let sidebar = o.shell.read_with(cx, |s, _| s.sidebar().cloned().unwrap());
    sidebar.update(cx, |s, cx| s.select(0, cx));
    for keys in ["down", "end", "right"] {
        press(cx, &o, keys);
    }
    assert_eq!(sel(cx, &o), "conn:0");
    assert!(tree(cx, o.win).is_empty(), "Right expanded nothing");
    // In the filter input the arrows stay the input's own.
    let filter = sidebar.read_with(cx, |s, _| s.filter_state().clone());
    cx.update_window(o.win, |_, w, cx| filter.update(cx, |f, cx| f.focus(w, cx)))
        .unwrap();
    for keys in ["down", "end", "right"] {
        press(cx, &o, keys);
    }
    assert_eq!(sel(cx, &o), "conn:0");
    assert!(tree(cx, o.win).is_empty());
}

#[gpui_kit::test]
fn enter_still_opens_a_table_reached_by_keys(cx: &mut TestAppContext) {
    let o = expanded(cx, T);
    for _ in 0..3 {
        press(cx, &o, "down");
    }
    assert_eq!(sel(cx, &o), "rel:Album");
    press(cx, &o, "enter");
    o.wait_browse(cx, "SELECT *\nFROM main.Album\nORDER BY AlbumId ASC");
}
