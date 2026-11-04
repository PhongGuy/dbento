//! The browse tab's filter row, header sort and the chip that says why it is not editable.
mod support;
use dbento_core::{BrowseError, RelationKind};
use gpui_kit::component::table::TableSelection;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext};
use support::{Opened, label_of, launched, opened, present, wait_real};

const T: &str = "CREATE TABLE Album(AlbumId INTEGER PRIMARY KEY, Title TEXT NOT NULL); \
                 CREATE TABLE shadow(rowid, _rowid_, oid); \
                 INSERT INTO Album(Title) VALUES ('b'), ('a'), ('c');";
const ALBUM: &str = "SELECT *\nFROM main.Album\nORDER BY AlbumId ASC";

fn album(cx: &mut TestAppContext, extra: &str) -> Opened {
    let o = launched(cx, T, extra);
    o.open_relation(cx, "Album", RelationKind::Table);
    o.wait_browse(cx, ALBUM);
    o
}
fn type_filter(cx: &mut TestAppContext, o: &Opened, text: &str) {
    let input = o.shell.read_with(cx, |s, cx| {
        s.tab()
            .unwrap()
            .read(cx)
            .results_pane()
            .read(cx)
            .filter_input()
            .clone()
    });
    cx.update_window(o.win, |_, w, cx| {
        input.update(cx, |s, cx| s.focus(w, cx));
        w.input(text, cx);
        w.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
}
fn run_id(cx: &mut TestAppContext, o: &Opened) -> u64 {
    o.shell.read_with(cx, |s, cx| {
        s.tab().unwrap().read(cx).model().read(cx).run_id()
    })
}
fn click(cx: &mut TestAppContext, o: &Opened, id: impl Into<gpui_kit::ElementId>) {
    let id = id.into();
    cx.update_window(o.win, |_, w, cx| w.click(id, cx)).unwrap();
    cx.run_until_parked();
}

#[gpui_kit::test]
fn a_browse_tab_has_the_filter_row_and_a_query_tab_does_not(cx: &mut TestAppContext) {
    let o = album(cx, "");
    for id in ["browse-filter", "browse-filter-input"] {
        assert!(present(cx, o.win, id), "{id}");
    }
    assert_eq!(
        label_of(cx, o.win, "browse-filter-where").as_deref(),
        Some("WHERE")
    );
    assert!(!present(cx, o.win, "browse-filter-error"));
    cx.update_window(o.win, |_, w, cx| {
        w.render_frame(cx);
        let list = w.find("statement-list").bounds();
        let row = w.find("browse-filter").bounds();
        let grid = w.find("result-grid").bounds();
        assert_eq!(row.size.height, gpui_kit::px(32.));
        assert!(row.origin.y >= list.origin.y + list.size.height);
        assert!(grid.origin.y >= row.origin.y + row.size.height);
    })
    .unwrap();
    let shell = o.shell.clone();
    cx.update_window(o.win, |_, w, cx| {
        shell.update(cx, |s, cx| s.open_connection(0, w, cx))
    })
    .unwrap();
    cx.run_until_parked();
    assert!(!present(cx, o.win, "browse-filter"));
}

#[gpui_kit::test]
fn enter_in_the_filter_runs_it(cx: &mut TestAppContext) {
    let o = album(cx, "");
    type_filter(cx, &o, "AlbumId > 1");
    o.wait_browse(
        cx,
        "SELECT *\nFROM main.Album\nWHERE (\nAlbumId > 1\n)\nORDER BY AlbumId ASC",
    );
    assert_eq!(
        label_of(cx, o.win, "footer-rows").as_deref(),
        Some("2 rows")
    );
}

#[gpui_kit::test]
fn a_header_sort_resets_unapplied_filter_text(cx: &mut TestAppContext) {
    let o = album(cx, "");
    let input = o.shell.read_with(cx, |s, cx| {
        s.tab()
            .unwrap()
            .read(cx)
            .results_pane()
            .read(cx)
            .filter_input()
            .clone()
    });
    cx.update_window(o.win, |_, w, cx| {
        input.update(cx, |s, cx| s.focus(w, cx));
        w.input("AlbumId > 1", cx);
    })
    .unwrap();
    click(cx, &o, ("result-col", 1usize));
    o.wait_browse(
        cx,
        "SELECT *\nFROM main.Album\nORDER BY Title ASC, AlbumId ASC",
    );
    assert_eq!(input.read_with(cx, |s, _| s.value().to_string()), "");
}

#[gpui_kit::test]
fn a_rejected_filter_shows_why_and_runs_nothing_until_cleared(cx: &mut TestAppContext) {
    let o = album(cx, "");
    let before = run_id(cx, &o);
    type_filter(cx, &o, "AlbumId > 1) /*");
    assert_eq!(
        label_of(cx, o.win, "browse-filter-error"),
        Some(BrowseError::Unterminated.to_string())
    );
    assert_eq!(run_id(cx, &o), before);
    click(cx, &o, "browse-filter-clear");
    assert!(!present(cx, o.win, "browse-filter-error"));
    wait_real(cx, "the rerun", |cx| run_id(cx, &o) > before);
    o.wait_browse(cx, ALBUM);
}

#[gpui_kit::test]
fn clicking_a_header_cycles_sort_and_shows_the_arrow(cx: &mut TestAppContext) {
    let o = album(cx, "");
    click(cx, &o, ("result-col", 1usize));
    o.wait_browse(
        cx,
        "SELECT *\nFROM main.Album\nORDER BY Title ASC, AlbumId ASC",
    );
    assert_eq!(
        label_of(cx, o.win, ("result-sort", 1usize)).as_deref(),
        Some("ascending")
    );
    assert_eq!(label_of(cx, o.win, "cell-0-1").as_deref(), Some("a"));
    click(cx, &o, ("result-col", 1usize));
    o.wait_browse(
        cx,
        "SELECT *\nFROM main.Album\nORDER BY Title DESC, AlbumId ASC",
    );
    assert_eq!(
        label_of(cx, o.win, ("result-sort", 1usize)).as_deref(),
        Some("descending")
    );
    assert_eq!(label_of(cx, o.win, "cell-0-1").as_deref(), Some("c"));
    let selection = o.shell.read_with(cx, |s, cx| {
        let pane = s.tab().unwrap().read(cx).results_pane().read(cx);
        pane.table().read(cx).selection()
    });
    assert_eq!(
        selection,
        TableSelection::None,
        "a sort click selects nothing"
    );
    click(cx, &o, ("result-col", 1usize));
    o.wait_browse(cx, ALBUM);
    assert!(!present(cx, o.win, ("result-sort", 1usize)));
}

#[gpui_kit::test]
fn a_free_tab_header_click_sorts_loaded_rows_and_selects_nothing(cx: &mut TestAppContext) {
    let o = opened(cx, T, "", 0);
    o.run(cx, "SELECT AlbumId, Title FROM Album");
    o.idle(cx);
    click(cx, &o, ("result-col", 1usize));
    let selection = o.shell.read_with(cx, |s, cx| {
        let pane = s.tab().unwrap().read(cx).results_pane().read(cx);
        pane.table().read(cx).selection()
    });
    assert_eq!(
        selection,
        TableSelection::None,
        "a sort click selects nothing"
    );
    assert!(present(cx, o.win, ("result-sort", 1usize)));
    assert!(present(cx, o.win, "local-sort"));
}

#[gpui_kit::test]
fn an_editable_browse_tab_shows_no_chip(cx: &mut TestAppContext) {
    let o = album(cx, "");
    assert!(!present(cx, o.win, "browse-not-editable"));
}

#[gpui_kit::test]
fn the_chip_names_a_read_only_connection(cx: &mut TestAppContext) {
    let o = album(cx, "read_only = true");
    assert_eq!(
        label_of(cx, o.win, "browse-not-editable").as_deref(),
        Some("read-only connection")
    );
}

#[gpui_kit::test]
fn the_chip_names_a_table_without_identity(cx: &mut TestAppContext) {
    let o = launched(cx, T, "");
    o.open_relation(cx, "shadow", RelationKind::Table);
    o.wait_browse(cx, "SELECT *\nFROM main.shadow");
    assert_eq!(
        label_of(cx, o.win, "browse-not-editable").as_deref(),
        Some("no row identity")
    );
}

#[gpui_kit::test]
fn a_locator_header_sorts_a_table_without_a_primary_key(cx: &mut TestAppContext) {
    let o = launched(
        cx,
        "CREATE TABLE plain(x); INSERT INTO plain VALUES ('b'), ('a');",
        "",
    );
    o.open_relation(cx, "plain", RelationKind::Table);
    o.wait_browse(cx, "SELECT rowid, *\nFROM main.plain\nORDER BY rowid ASC");
    click(cx, &o, ("result-col", 0usize));
    o.wait_browse(cx, "SELECT rowid, *\nFROM main.plain\nORDER BY rowid ASC");
    assert_eq!(
        label_of(cx, o.win, ("result-sort", 0usize)).as_deref(),
        Some("ascending")
    );
}

#[gpui_kit::test]
fn the_filter_clear_shows_only_with_text(cx: &mut TestAppContext) {
    let o = album(cx, "");
    assert!(!present(cx, o.win, "browse-filter-clear"));
    let input = o.shell.read_with(cx, |s, cx| {
        s.tab()
            .unwrap()
            .read(cx)
            .results_pane()
            .read(cx)
            .filter_input()
            .clone()
    });
    cx.update_window(o.win, |_, w, cx| {
        input.update(cx, |s, cx| s.focus(w, cx));
        w.input("AlbumId > 1", cx);
    })
    .unwrap();
    cx.run_until_parked();
    assert!(present(cx, o.win, "browse-filter-clear"));
    click(cx, &o, "browse-filter-clear");
    assert!(!present(cx, o.win, "browse-filter-clear"));
}

#[gpui_kit::test]
fn a_postgres_xmin_header_stays_plain_while_ctid_sorts(cx: &mut TestAppContext) {
    // The database's own runtime, kept entered, since the shell starts the app's bridge.
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let db = rt.block_on(dbento_testkit::pg_db());
    rt.block_on(async {
        db.admin()
            .await
            .batch_execute("CREATE TABLE nokey(a int, b text); INSERT INTO nokey VALUES (2, 'x');")
            .await
            .unwrap()
    });
    let password = db.password().to_owned();
    let s = support::started_custom(
        cx,
        &format!("dbento = 1\n{}", db.connection_toml("pg", "")),
        move |sv| {
            sv.env = std::sync::Arc::new(move |v| {
                (v == dbento_testkit::SECRET_VAR).then(|| password.clone())
            })
        },
    );
    let shell = s.shell.clone();
    let rel = dbento_core::RelationRef::new("public", "nokey");
    cx.update_window(s.win, |_, w, cx| {
        shell.update(cx, |s, cx| {
            s.open_relation(0, &rel, RelationKind::Table, w, cx)
        })
    })
    .unwrap();
    let model = |cx: &mut TestAppContext| {
        s.shell
            .read_with(cx, |s, cx| s.tab().unwrap().read(cx).model().clone())
    };
    wait_real(cx, "the browse", |cx| {
        let m = model(cx);
        m.read_with(cx, |m, _| m.editable() && !m.running())
    });
    // Keyless Postgres rows are located by ctid then xmin, shown first.
    assert_eq!(
        label_of(cx, s.win, ("result-col", 0usize)).as_deref(),
        Some("ctid")
    );
    assert_eq!(
        label_of(cx, s.win, ("result-col", 1usize)).as_deref(),
        Some("xmin")
    );
    let run_id = |cx: &mut TestAppContext| model(cx).read_with(cx, |m, _| m.run_id());
    let before = run_id(cx);
    cx.update_window(s.win, |_, w, cx| w.click(("result-col", 1usize), cx))
        .unwrap();
    cx.run_until_parked();
    assert_eq!(
        run_id(cx),
        before,
        "xmin has no ordering, so its click runs nothing"
    );
    assert!(!present(cx, s.win, ("result-sort", 1usize)));
    cx.update_window(s.win, |_, w, cx| w.click(("result-col", 0usize), cx))
        .unwrap();
    cx.run_until_parked();
    wait_real(cx, "the ctid sort", |cx| {
        let m = model(cx);
        m.read_with(cx, |m, _| m.run_id() > before && !m.running())
            && present(cx, s.win, ("result-sort", 0usize))
    });
}
