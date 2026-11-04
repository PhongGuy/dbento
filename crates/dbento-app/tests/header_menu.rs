//! The header menu on browse and query grids.
mod support;
use dbento_app::grid_menu::{GridTarget, MenuEntry, SectionKind, menu_sections};
use dbento_core::RelationKind;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, ElementId, TestAppContext};
use support::{Opened, fixture, label_of, launched, opened, present, ready, wait_real};

const T: &str = "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT NOT NULL, note TEXT); \
                 INSERT INTO t VALUES (1, 'b', 'x'), (2, 'a', NULL);";
const COLUMN_IDS: [&str; 7] = [
    "header-menu-sort-asc",
    "header-menu-sort-desc",
    "header-menu-sort-clear",
    "header-menu-copy-name",
    "header-menu-hide",
    "header-menu-show-all",
    "header-menu-fit",
];

fn ids(e: &[MenuEntry]) -> Vec<&'static str> {
    e.iter().map(|e| e.id).collect()
}
fn enabled(e: &[MenuEntry]) -> Vec<&'static str> {
    e.iter().filter(|e| e.enabled).map(|e| e.id).collect()
}
fn menu(cx: &mut TestAppContext, o: &Opened, right: impl Into<ElementId>, item: &'static str) {
    let right = right.into();
    cx.update_window(o.win, |_, w, cx| w.right_click(right, cx))
        .unwrap();
    cx.run_until_parked();
    cx.update_window(o.win, |_, w, cx| w.click(item, cx))
        .unwrap();
    cx.run_until_parked();
}
fn filter_text(cx: &mut TestAppContext, o: &Opened) -> String {
    o.shell.read_with(cx, |s, cx| {
        let pane = s.tab().unwrap().read(cx).results_pane().read(cx);
        pane.filter_input().read(cx).value().to_string()
    })
}

#[gpui_kit::test]
fn a_browse_header_offers_column_filter_edit_and_copy(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = ready(cx, &fx, "t");
    m.read_with(cx, |m, cx| {
        let s = menu_sections(m, 0, GridTarget::Header(2), cx);
        let kinds: Vec<_> = s.iter().map(|s| s.kind).collect();
        assert_eq!(
            kinds,
            [
                SectionKind::Column,
                SectionKind::Edit,
                SectionKind::Filter,
                SectionKind::Copy
            ]
        );
        assert_eq!(ids(&s[0].entries), COLUMN_IDS);
        assert_eq!(s[0].entries[0].label, "Sort ascending");
        assert_eq!(
            enabled(&s[0].entries),
            [
                "header-menu-sort-asc",
                "header-menu-sort-desc",
                "header-menu-copy-name",
                "header-menu-hide",
                "header-menu-fit",
            ]
        );
        assert_eq!(ids(&s[2].entries), ["header-menu-filter"]);
        assert_eq!(s[2].entries[0].label, "Filter by\u{2026}");
        let cell = menu_sections(m, 0, GridTarget::Cell(0, 2), cx);
        assert!(!cell.iter().any(|s| s.kind == SectionKind::Column));
    });
}

#[gpui_kit::test]
fn a_query_header_sorts_the_loaded_rows_and_says_so(cx: &mut TestAppContext) {
    let o = opened(cx, T, "", 0);
    o.run(cx, "SELECT id, name FROM t ORDER BY id");
    o.idle(cx);
    o.model(cx).read_with(cx, |m, cx| {
        let s = menu_sections(m, 0, GridTarget::Header(2), cx);
        let kinds: Vec<_> = s.iter().map(|s| s.kind).collect();
        assert_eq!(kinds, [SectionKind::Column, SectionKind::Copy]);
        let labels: Vec<_> = s[0].entries[..3].iter().map(|e| e.label).collect();
        assert_eq!(
            labels,
            [
                "Sort loaded rows ascending",
                "Sort loaded rows descending",
                "Restore loaded order"
            ]
        );
    });
    menu(cx, &o, ("result-col", 1usize), "header-menu-sort-asc");
    assert_eq!(label_of(cx, o.win, "cell-0-1").as_deref(), Some("a"));
    assert_eq!(
        label_of(cx, o.win, "local-sort").as_deref(),
        Some("Loaded rows sorted by name ascending")
    );
    assert!(present(cx, o.win, ("result-sort", 1usize)));
    menu(cx, &o, ("result-col", 1usize), "header-menu-sort-clear");
    assert_eq!(label_of(cx, o.win, "cell-0-1").as_deref(), Some("b"));
    assert!(!present(cx, o.win, "local-sort"));
}

#[gpui_kit::test]
fn browse_header_items_sort_prefill_copy_hide_and_show(cx: &mut TestAppContext) {
    let o = launched(cx, T, "");
    o.open_relation(cx, "t", RelationKind::Table);
    o.wait_browse(cx, "SELECT *\nFROM main.t\nORDER BY id ASC");
    let m = o.model(cx);
    menu(cx, &o, ("result-col", 1usize), "header-menu-sort-asc");
    wait_real(cx, "the sorted browse", |cx| {
        m.read_with(cx, |m, _| {
            !m.running()
                && m.browse()
                    .is_some_and(|b| b.sql().contains("ORDER BY name ASC"))
        })
    });
    assert_eq!(label_of(cx, o.win, "cell-0-1").as_deref(), Some("a"));
    menu(cx, &o, ("result-col", 1usize), "header-menu-copy-name");
    assert_eq!(
        cx.read_from_clipboard().and_then(|c| c.text()).as_deref(),
        Some("name")
    );
    menu(cx, &o, ("result-col", 1usize), "header-menu-filter");
    assert_eq!(filter_text(cx, &o), "name = ");
    m.read_with(cx, |m, _| {
        assert_eq!(m.browse().unwrap().browse().filter, None, "nothing ran")
    });
    menu(cx, &o, ("result-col", 2usize), "header-menu-hide");
    assert!(!present(cx, o.win, ("result-col", 2usize)));
    menu(cx, &o, ("result-col", 1usize), "header-menu-show-all");
    assert!(present(cx, o.win, ("result-col", 2usize)));
}
