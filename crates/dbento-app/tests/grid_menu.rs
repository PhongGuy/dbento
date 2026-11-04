//! Which grid menu items a cell or row offers.
mod support;
use dbento_app::grid_menu::{
    GridTarget, MenuEntry, MenuSection, SectionKind, insert_table, menu_entries, menu_sections,
};
use dbento_app::keymap::{COPY_CELLS_KEYS, DELETE_ROW_KEYS, EDIT_CELL_KEYS, chip_keys};
use dbento_app::staged::GridRow::{Inserted, Loaded};
use dbento_core::{Engine, RelationRef};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext};
use support::{browsing, fixture, ready, wait_prompt, wait_real};

const T: &str = "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT NOT NULL, note TEXT); \
                 INSERT INTO t VALUES (1, 'b', 'x'), (2, 'a', NULL);";
const COPIES: [&str; 3] = [
    "grid-menu-copy-insert",
    "grid-menu-copy-update",
    "grid-menu-copy-delete",
];

const COPY_IDS: [&str; 7] = [
    "grid-menu-copy",
    "grid-menu-copy-headers",
    "grid-menu-copy-csv",
    "grid-menu-copy-json",
    "grid-menu-copy-markdown",
    "grid-menu-copy-sql",
    "grid-menu-export",
];

fn kinds(s: &[MenuSection]) -> Vec<SectionKind> {
    s.iter().map(|s| s.kind).collect()
}

fn enabled(entries: &[MenuEntry]) -> Vec<&'static str> {
    entries.iter().filter(|e| e.enabled).map(|e| e.id).collect()
}

#[gpui_kit::test]
fn a_loaded_cell_offers_what_applies_to_it(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = ready(cx, &fx, "t");
    m.read_with(cx, |m, cx| {
        let all = menu_entries(m, Some(Loaded(0)), Some(2), cx);
        let ids: Vec<_> = all.iter().map(|e| e.id).collect();
        assert_eq!(
            ids,
            [
                "grid-menu-edit",
                "grid-menu-set-null",
                "grid-menu-set-default",
                "grid-menu-add-row",
                "grid-menu-duplicate-row",
                "grid-menu-delete-row",
                "grid-menu-revert-row",
                "grid-menu-copy-insert",
                "grid-menu-copy-update",
                "grid-menu-copy-delete",
            ]
        );
        assert_eq!(
            all.iter().map(|e| e.label).collect::<Vec<_>>(),
            [
                "Edit",
                "Set NULL",
                "Set DEFAULT",
                "Add row",
                "Duplicate row",
                "Delete row",
                "Revert row",
                "Copy row as INSERT",
                "Copy row as UPDATE",
                "Copy row as DELETE",
            ]
        );
        assert_eq!(
            enabled(&all),
            [
                "grid-menu-edit",
                "grid-menu-set-null",
                "grid-menu-set-default",
                "grid-menu-add-row",
                "grid-menu-duplicate-row",
                "grid-menu-delete-row",
                COPIES[0],
                COPIES[1],
                COPIES[2],
            ],
            "nothing to revert yet"
        );
        assert!(
            !enabled(&menu_entries(m, Some(Loaded(0)), Some(1), cx))
                .contains(&"grid-menu-set-null"),
            "name is NOT NULL"
        );
        assert!(
            !enabled(&menu_entries(m, Some(Loaded(1)), Some(2), cx))
                .contains(&"grid-menu-set-null"),
            "already NULL"
        );
        assert_eq!(
            enabled(&menu_entries(m, Some(Loaded(0)), None, cx)),
            [
                "grid-menu-add-row",
                "grid-menu-duplicate-row",
                "grid-menu-delete-row",
                COPIES[0],
                COPIES[1],
                COPIES[2],
            ],
            "the gutter offers row items"
        );
        assert_eq!(
            enabled(&menu_entries(m, None, None, cx)),
            ["grid-menu-add-row"]
        );
    });
}

#[gpui_kit::test]
fn deleted_and_inserted_rows_change_the_offer(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = ready(cx, &fx, "t");
    m.update(cx, |m, cx| {
        assert!(m.delete_row(1, cx));
        assert_eq!(m.add_row(cx), Some(0));
    });
    m.read_with(cx, |m, cx| {
        assert_eq!(
            enabled(&menu_entries(m, Some(Loaded(1)), Some(2), cx)),
            [
                "grid-menu-add-row",
                "grid-menu-duplicate-row",
                "grid-menu-revert-row",
                COPIES[0],
                COPIES[1],
                COPIES[2],
            ],
            "a deleted row"
        );
        assert_eq!(
            enabled(&menu_entries(m, Some(Inserted(0)), Some(2), cx)),
            [
                "grid-menu-edit",
                "grid-menu-set-null",
                "grid-menu-add-row",
                "grid-menu-duplicate-row",
                "grid-menu-delete-row",
                "grid-menu-revert-row",
                COPIES[0],
            ],
            "an inserted row"
        );
    });
}

#[gpui_kit::test]
fn a_read_only_browse_offers_only_copies(cx: &mut TestAppContext) {
    let fx = fixture(T, "read_only = true");
    let m = browsing(cx, &fx, "t", Default::default(), "");
    wait_real(cx, "the browse", |cx| {
        m.read_with(cx, |m, _| m.edit_block().is_some() && !m.running())
    });
    m.read_with(cx, |m, cx| {
        assert_eq!(
            enabled(&menu_entries(m, Some(Loaded(0)), Some(2), cx)),
            COPIES
        );
    });
}

#[gpui_kit::test]
fn a_free_query_has_no_menu(cx: &mut TestAppContext) {
    let o = support::opened(cx, T, "", 0);
    o.run(cx, "SELECT * FROM t");
    o.idle(cx);
    let m = o.model(cx);
    assert!(m.read_with(cx, |m, cx| {
        menu_entries(m, Some(Loaded(0)), Some(1), cx).is_empty()
    }));
}

#[gpui_kit::test]
fn a_save_in_flight_offers_no_row_operations(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = ready(cx, &fx, "t");
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 1, "z", cx)));
    assert!(m.update(cx, |m, cx| m.save(cx)));
    wait_prompt(cx, &m);
    m.read_with(cx, |m, cx| {
        let on = enabled(&menu_entries(m, Some(Loaded(0)), Some(1), cx));
        for id in [
            "grid-menu-add-row",
            "grid-menu-duplicate-row",
            "grid-menu-delete-row",
            "grid-menu-revert-row",
        ] {
            assert!(!on.contains(&id), "{id} while the apply prompt waits");
        }
    });
}

#[gpui_kit::test]
fn edit_and_delete_carry_their_keys(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = ready(cx, &fx, "t");
    m.read_with(cx, |m, cx| {
        let keyed: Vec<_> = menu_entries(m, Some(Loaded(0)), Some(2), cx)
            .iter()
            .filter_map(|e| e.keys.clone().map(|k| (e.id, k)))
            .collect();
        assert_eq!(
            keyed,
            [
                ("grid-menu-edit", EDIT_CELL_KEYS[0].into()),
                ("grid-menu-delete-row", DELETE_ROW_KEYS.into()),
            ]
        );
    });
}

#[gpui_kit::test]
fn a_free_query_gets_the_copy_section_only(cx: &mut TestAppContext) {
    let o = support::opened(cx, T, "", 0);
    o.run(cx, "SELECT * FROM t");
    o.idle(cx);
    o.model(cx).read_with(cx, |m, cx| {
        let s = menu_sections(m, 0, GridTarget::Cell(0, 1), cx);
        assert_eq!(kinds(&s), [SectionKind::Copy]);
        assert_eq!(
            s[0].entries.iter().map(|e| e.id).collect::<Vec<_>>(),
            COPY_IDS
        );
        assert_eq!(enabled(&s[0].entries), COPY_IDS, "single_source names t");
        assert_eq!(s[0].entries[0].keys, Some(COPY_CELLS_KEYS.into()));
        let below = menu_sections(m, 0, GridTarget::Below, cx);
        assert_eq!(
            enabled(&below[0].entries),
            ["grid-menu-export"],
            "nothing to copy below the rows, but the rows export"
        );
    });
    o.run(cx, "SELECT a.id FROM t a JOIN t b ON a.id = b.id");
    o.idle(cx);
    o.model(cx).read_with(cx, |m, cx| {
        let s = menu_sections(m, 0, GridTarget::Header(1), cx);
        assert_eq!(kinds(&s), [SectionKind::Column, SectionKind::Copy]);
        assert!(
            !enabled(&s[1].entries).contains(&"grid-menu-copy-sql"),
            "no single table"
        );
    });
}

#[gpui_kit::test]
fn a_browse_grid_gets_edit_then_copy(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = ready(cx, &fx, "t");
    m.read_with(cx, |m, cx| {
        let s = menu_sections(m, 0, GridTarget::Cell(0, 2), cx);
        assert_eq!(
            kinds(&s),
            [SectionKind::Edit, SectionKind::Filter, SectionKind::Copy]
        );
        assert_eq!(s[0].entries, menu_entries(m, Some(Loaded(0)), Some(1), cx));
        let header = menu_sections(m, 0, GridTarget::Header(2), cx);
        assert_eq!(
            header[1].kind,
            SectionKind::Edit,
            "after the header's Column section"
        );
        let ids: Vec<_> = header[1].entries.iter().map(|e| e.id).collect();
        assert_eq!(ids, ["grid-menu-add-row"]);
    });
}

#[gpui_kit::test]
fn a_query_cell_copies_as_csv_and_a_header_copies_its_column(cx: &mut TestAppContext) {
    let o = support::opened(cx, T, "", 0);
    o.run(cx, "SELECT id, name FROM t ORDER BY id");
    o.idle(cx);
    let clip = |cx: &mut TestAppContext| cx.read_from_clipboard().and_then(|c| c.text());
    let act = |cx: &mut TestAppContext, right: gpui_kit::ElementId, item: &'static str| {
        cx.update_window(o.win, |_, w, cx| w.right_click(right, cx))
            .unwrap();
        cx.run_until_parked();
        cx.update_window(o.win, |_, w, cx| w.click(item, cx))
            .unwrap();
        cx.run_until_parked();
    };
    cx.update_window(o.win, |_, w, cx| w.right_click("cell-0-1", cx))
        .unwrap();
    cx.run_until_parked();
    assert_eq!(
        support::label_of(cx, o.win, "grid-menu-copy-keys"),
        Some(chip_keys(COPY_CELLS_KEYS))
    );
    cx.update_window(o.win, |_, w, cx| w.click("grid-menu-copy-csv", cx))
        .unwrap();
    cx.run_until_parked();
    assert_eq!(clip(cx).as_deref(), Some("name\nb"));
    act(cx, ("result-col", 1usize).into(), "grid-menu-copy-json");
    assert_eq!(
        clip(cx).as_deref(),
        Some("[\n  {\"name\": \"b\"},\n  {\"name\": \"a\"}\n]")
    );
}

#[gpui_kit::test]
fn a_mysql_select_offers_its_table_in_the_connections_database(cx: &mut TestAppContext) {
    support::init(cx);
    support::init_tokio(cx);
    let rt = cx.update(|cx| dbento_app::tokio_bridge::handle(cx));
    let _enter = rt.enter();
    let db = rt.block_on(dbento_testkit::mysql_db());
    let (_dir, spec) = support::my_spec(&db, "");
    let m = cx.new(|_| dbento_app::tab_model::TabModel::new(spec));
    let sql =
        "CREATE TABLE t (id INT PRIMARY KEY); SELECT * FROM t; SELECT * FROM t JOIN t u USING (id)";
    m.update(cx, |m, cx| m.run(sql.into(), cx));
    wait_real(cx, "the run", |cx| m.read_with(cx, |m, _| !m.running()));
    m.read_with(cx, |m, _| {
        assert_eq!(
            insert_table(m, 1),
            Some((Engine::Mysql, RelationRef::new(db.database(), "t")))
        );
        assert_eq!(insert_table(m, 2), None, "a join names no table");
    });
}
