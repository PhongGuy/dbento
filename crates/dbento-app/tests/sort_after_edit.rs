//! A table tab whose query the user edited still sorts by its column headers.
use crate::support::{label_of, launched, present};
use dbento_core::RelationKind;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext};

const T: &str = "CREATE TABLE Album(AlbumId INTEGER PRIMARY KEY, Title TEXT NOT NULL); \
                 INSERT INTO Album(Title) VALUES ('b'), ('a'), ('c');";
const ALBUM: &str = "SELECT *\nFROM main.Album\nORDER BY AlbumId ASC";

#[gpui_kit::test]
fn an_edited_table_query_still_sorts_by_header_click(cx: &mut TestAppContext) {
    let o = launched(cx, T, "");
    o.open_relation(cx, "Album", RelationKind::Table);
    o.wait_browse(cx, ALBUM);
    // A keystroke in the editor is the user changing the table's query.
    let e = o
        .shell
        .read_with(cx, |s, cx| s.tab().unwrap().read(cx).editor().clone());
    cx.update_window(o.win, |_, w, cx| {
        e.update(cx, |e, cx| e.focus(w, cx));
        w.input(" ", cx);
    })
    .unwrap();
    cx.run_until_parked();
    o.run(cx, "SELECT * FROM main.Album WHERE AlbumId > 0");
    o.idle(cx);
    assert!(
        o.model(cx).read_with(cx, |m, _| m.browse().is_none()),
        "the edit left the browse"
    );
    assert_eq!(label_of(cx, o.win, "cell-0-1").as_deref(), Some("b"));

    cx.update_window(o.win, |_, w, cx| w.click(("result-col", 1usize), cx))
        .unwrap();
    cx.run_until_parked();
    o.idle(cx);
    assert!(
        present(cx, o.win, ("result-sort", 1usize)),
        "the header click shows a sort arrow"
    );
    assert_eq!(label_of(cx, o.win, "cell-0-1").as_deref(), Some("a"));
    assert_eq!(
        label_of(cx, o.win, "local-sort").as_deref(),
        Some("Loaded rows sorted by Title ascending")
    );
    cx.update_window(o.win, |_, w, cx| w.click(("result-col", 1usize), cx))
        .unwrap();
    cx.run_until_parked();
    assert_eq!(label_of(cx, o.win, "cell-0-1").as_deref(), Some("c"));
    assert_eq!(
        label_of(cx, o.win, "local-sort").as_deref(),
        Some("Loaded rows sorted by Title descending")
    );
    cx.update_window(o.win, |_, w, cx| w.click(("result-col", 1usize), cx))
        .unwrap();
    cx.run_until_parked();
    assert_eq!(
        label_of(cx, o.win, "cell-0-1").as_deref(),
        Some("b"),
        "loaded order again"
    );
    assert!(!present(cx, o.win, "local-sort"));
    assert!(!present(cx, o.win, ("result-sort", 1usize)));
}
