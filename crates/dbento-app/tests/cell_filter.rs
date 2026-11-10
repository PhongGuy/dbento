//! A browse cell filters by its value into the WHERE bar, behind the discard guard.
use crate::support;
use crate::support::{fixture, label_of, launched, opened, present, ready, wait_real};
use dbento_app::browse::and_filter;
use dbento_app::grid_menu::{
    GridTarget, MenuEntry, SectionKind, filter_clause, filter_label, menu_sections,
};
use dbento_app::tab_model::Discard;
use dbento_core::{Engine, RelationKind};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext};

const T: &str = "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT NOT NULL, note TEXT); \
                 INSERT INTO t VALUES (1, 'b', 'x'), (2, 'a', NULL);";
const FILTER_IDS: [&str; 3] = [
    "grid-menu-filter-eq",
    "grid-menu-filter-ne",
    "grid-menu-filter-null",
];

fn enabled(e: &[MenuEntry]) -> Vec<&'static str> {
    e.iter().filter(|e| e.enabled).map(|e| e.id).collect()
}

#[test]
fn and_filter_joins_and_parenthesises_an_or() {
    let and = |e: Option<&str>, c: &str| and_filter(Engine::Sqlite, e, c).unwrap();
    assert_eq!(and(None, "a = 1"), "a = 1");
    assert_eq!(and(Some("  "), "a = 1"), "a = 1");
    assert_eq!(and(Some("id > 1"), "a = 1"), "id > 1 AND a = 1");
    assert_eq!(
        and(Some("id = 1 or id = 2"), "a = 1"),
        "(id = 1 or id = 2) AND a = 1"
    );
    assert_eq!(
        and(Some("color = 'red'"), "a = 1"),
        "color = 'red' AND a = 1"
    );
    // The WHERE bar is one line, so a trailing line comment must stay last.
    assert_eq!(
        and(Some("id > 1 -- recent"), "a = 1"),
        "a = 1 AND id > 1 -- recent"
    );
    assert_eq!(
        and(Some("id = 1 or id = 2 -- two"), "a = 1"),
        "a = 1 AND (id = 1 or id = 2) -- two"
    );
}

#[test]
fn and_filter_sees_or_and_comments_only_as_tokens() {
    let and = |e: &str| and_filter(Engine::Sqlite, Some(e), "a = 1").unwrap();
    assert_eq!(and("name = 'or'"), "name = 'or' AND a = 1");
    assert_eq!(
        and("name = 'this or that'"),
        "name = 'this or that' AND a = 1"
    );
    assert_eq!(
        and("\"color or size\" = 1"),
        "\"color or size\" = 1 AND a = 1"
    );
    assert_eq!(and("id > 1 /* or not */"), "id > 1 /* or not */ AND a = 1");
    assert_eq!(and("note = 'a--b'"), "note = 'a--b' AND a = 1");
    assert_eq!(
        and("note = 'a--b' or id = 2"),
        "(note = 'a--b' or id = 2) AND a = 1"
    );
    assert_eq!(and("name = 'or' -- c"), "a = 1 AND name = 'or' -- c");
    assert_eq!(
        and("id = 1 OR id = 2 -- or"),
        "a = 1 AND (id = 1 OR id = 2) -- or"
    );
    assert_eq!(
        and("id = 1 /* a */ or id = 2  -- b"),
        "a = 1 AND (id = 1 /* a */ or id = 2)  -- b"
    );
    let pg = and_filter(Engine::Postgres, Some("note = $$ or $$"), "a = 1");
    assert_eq!(pg.as_deref(), Some("note = $$ or $$ AND a = 1"));
    // An open quote or block comment would swallow the clause.
    assert_eq!(and_filter(Engine::Sqlite, Some("name = 'x"), "a = 1"), None);
    assert_eq!(
        and_filter(Engine::Sqlite, Some("id > 1 /* x"), "a = 1"),
        None
    );
}

#[test]
fn a_long_clause_is_cut_in_the_label() {
    assert_eq!(filter_label("name = 'b'"), "Filter: name = 'b'");
    let long = format!("name = '{}'", "x".repeat(60));
    assert_eq!(
        filter_label(&long),
        format!("Filter: {}\u{2026}", &long[..40])
    );
}

#[gpui_kit::test]
fn a_browse_cell_offers_the_filters_that_apply(cx: &mut TestAppContext) {
    let fx = fixture(T, "");
    let m = ready(cx, &fx, "t");
    m.read_with(cx, |m, cx| {
        let s = menu_sections(m, 0, GridTarget::Cell(1, 3), cx);
        let kinds: Vec<_> = s.iter().map(|s| s.kind).collect();
        assert_eq!(
            kinds,
            [SectionKind::Edit, SectionKind::Filter, SectionKind::Copy]
        );
        assert_eq!(
            s[1].entries.iter().map(|e| e.id).collect::<Vec<_>>(),
            FILTER_IDS
        );
        assert_eq!(
            enabled(&s[1].entries),
            ["grid-menu-filter-null"],
            "a NULL cell"
        );
        let null = GridTarget::Cell(1, 3);
        assert_eq!(
            filter_clause(m, 0, null, "grid-menu-filter-null").as_deref(),
            Some("note IS NULL")
        );
        let b = GridTarget::Cell(0, 2);
        assert_eq!(enabled(&menu_sections(m, 0, b, cx)[1].entries), FILTER_IDS);
        assert_eq!(
            filter_clause(m, 0, b, "grid-menu-filter-eq").as_deref(),
            Some("name = 'b'")
        );
        assert_eq!(
            filter_clause(m, 0, b, "grid-menu-filter-ne").as_deref(),
            Some("name <> 'b'")
        );
    });
    assert_eq!(m.update(cx, |m, cx| m.add_row(cx)), Some(0));
    m.read_with(cx, |m, cx| {
        let s = menu_sections(m, 0, GridTarget::Cell(0, 2), cx);
        assert!(
            enabled(&s[1].entries).is_empty(),
            "an inserted row has no loaded value"
        );
    });
}

#[gpui_kit::test]
fn a_cell_filter_on_a_commented_filter_survives_the_where_bar(cx: &mut TestAppContext) {
    let o = launched(cx, T, "");
    o.open_relation(cx, "t", RelationKind::Table);
    o.wait_browse(cx, "SELECT *\nFROM main.t\nORDER BY id ASC");
    let m = o.model(cx);
    let rows = |cx: &mut TestAppContext, f: &'static str, n: u64| {
        wait_real(cx, f, |cx| {
            m.read_with(cx, |m, _| {
                !m.running()
                    && m.browse()
                        .is_some_and(|b| b.browse().filter.as_deref() == Some(f))
                    && m.results().first().is_some_and(|r| r.store.rows() == n)
            })
        })
    };
    assert!(m.update(cx, |m, cx| m.apply_filter("id > 0 -- all", cx)));
    rows(cx, "id > 0 -- all", 2);
    cx.update_window(o.win, |_, w, cx| w.right_click("cell-0-1", cx))
        .unwrap();
    cx.run_until_parked();
    cx.update_window(o.win, |_, w, cx| w.click("grid-menu-filter-eq", cx))
        .unwrap();
    cx.run_until_parked();
    wait_real(cx, "the cell filter", |cx| {
        m.read_with(cx, |m, _| {
            !m.running() && m.results().first().is_some_and(|r| r.store.rows() == 1)
        })
    });
    // The bar holds the applied filter as one line; Enter on it unedited re-applies it.
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
        w.press("enter", cx);
    })
    .unwrap();
    cx.run_until_parked();
    o.idle(cx);
    let (filter, n) = m.read_with(cx, |m, _| {
        let f = m.browse().and_then(|b| b.browse().filter.clone());
        (f, m.results().first().map(|r| r.store.rows()))
    });
    assert_eq!(
        (filter.as_deref(), n),
        (Some("name = 'b' AND id > 0 -- all"), Some(1))
    );
    // "Filter by..." prefills in front of the comment, the caret after the new clause.
    cx.update_window(o.win, |_, w, cx| w.right_click(("result-col", 1usize), cx))
        .unwrap();
    cx.run_until_parked();
    cx.update_window(o.win, |_, w, cx| w.click("header-menu-filter", cx))
        .unwrap();
    cx.run_until_parked();
    let (text, caret) = input.read_with(cx, |s, _| (s.value().to_string(), s.cursor()));
    assert_eq!(text, "name =  AND name = 'b' AND id > 0 -- all");
    assert_eq!(caret, "name = ".len());
}

#[gpui_kit::test]
fn a_commented_or_filter_takes_a_cell_filter(cx: &mut TestAppContext) {
    let o = launched(cx, T, "");
    o.open_relation(cx, "t", RelationKind::Table);
    o.wait_browse(cx, "SELECT *\nFROM main.t\nORDER BY id ASC");
    let m = o.model(cx);
    assert!(m.update(cx, |m, cx| m.apply_filter("id = 1 or id = 2 -- two", cx)));
    wait_real(cx, "the or filter", |cx| {
        m.read_with(cx, |m, _| {
            !m.running() && m.results().first().is_some_and(|r| r.store.rows() == 2)
        })
    });
    m.read_with(cx, |m, cx| {
        let header = menu_sections(m, 0, GridTarget::Header(1), cx);
        let by = header
            .iter()
            .find(|s| s.kind == SectionKind::Filter)
            .unwrap();
        assert_eq!(enabled(&by.entries), ["header-menu-filter"]);
    });
    cx.update_window(o.win, |_, w, cx| w.right_click("cell-0-1", cx))
        .unwrap();
    cx.run_until_parked();
    cx.update_window(o.win, |_, w, cx| w.click("grid-menu-filter-eq", cx))
        .unwrap();
    cx.run_until_parked();
    let want = "name = 'b' AND (id = 1 or id = 2) -- two";
    wait_real(cx, "the cell filter", |cx| {
        m.read_with(cx, |m, _| {
            !m.running()
                && m.browse()
                    .is_some_and(|b| b.browse().filter.as_deref() == Some(want))
                && m.results().first().is_some_and(|r| r.store.rows() == 1)
        })
    });
}

#[gpui_kit::test]
fn a_long_cell_menu_fits_a_short_window(cx: &mut TestAppContext) {
    let o = launched(cx, T, "");
    o.open_relation(cx, "t", RelationKind::Table);
    o.wait_browse(cx, "SELECT *\nFROM main.t\nORDER BY id ASC");
    let height = gpui_kit::px(420.);
    cx.simulate_window_resize(o.win, gpui_kit::size(gpui_kit::px(1280.), height));
    cx.run_until_parked();
    cx.update_window(o.win, |_, w, cx| w.right_click("cell-0-1", cx))
        .unwrap();
    cx.run_until_parked();
    cx.update_window(o.win, |_, w, cx| {
        w.render_frame(cx);
        let menu = w.find("popup-menu").bounds();
        assert!(
            menu.top() >= gpui_kit::px(0.) && menu.bottom() <= height,
            "{menu:?} in a {height:?} window"
        );
    })
    .unwrap();
}

#[gpui_kit::test]
fn the_drawn_menu_names_the_clause_each_filter_applies(cx: &mut TestAppContext) {
    let o = launched(cx, T, "");
    o.open_relation(cx, "t", RelationKind::Table);
    o.wait_browse(cx, "SELECT *\nFROM main.t\nORDER BY id ASC");
    cx.update_window(o.win, |_, w, cx| w.right_click("cell-0-1", cx))
        .unwrap();
    cx.run_until_parked();
    let label = |cx: &mut TestAppContext, id| label_of(cx, o.win, id);
    assert_eq!(
        label(cx, "grid-menu-filter-eq").as_deref(),
        Some("Filter: name = 'b'")
    );
    assert_eq!(
        label(cx, "grid-menu-filter-ne").as_deref(),
        Some("Filter: name <> 'b'")
    );
    assert_eq!(
        label(cx, "grid-menu-filter-null").as_deref(),
        Some("Filter: name IS NULL"),
        "disabled on a value, it still names its clause"
    );
}

#[gpui_kit::test]
fn a_query_cell_has_no_filter_section(cx: &mut TestAppContext) {
    let o = opened(cx, T, "", 0);
    o.run(cx, "SELECT * FROM t");
    o.idle(cx);
    o.model(cx).read_with(cx, |m, cx| {
        let s = menu_sections(m, 0, GridTarget::Cell(0, 2), cx);
        assert!(!s.iter().any(|s| s.kind == SectionKind::Filter));
    });
}

#[gpui_kit::test]
fn a_cell_filter_ands_into_the_where_bar_behind_the_guard(cx: &mut TestAppContext) {
    let o = launched(cx, T, "");
    o.open_relation(cx, "t", RelationKind::Table);
    o.wait_browse(cx, "SELECT *\nFROM main.t\nORDER BY id ASC");
    let m = o.model(cx);
    let applied = |cx: &mut TestAppContext, f: &'static str, rows: u64| {
        wait_real(cx, f, |cx| {
            m.read_with(cx, |m, _| {
                !m.running()
                    && m.browse()
                        .is_some_and(|b| b.browse().filter.as_deref() == Some(f))
                    && m.results().first().is_some_and(|r| r.store.rows() == rows)
            })
        })
    };
    assert!(m.update(cx, |m, cx| m.apply_filter("id > 0", cx)));
    applied(cx, "id > 0", 2);
    let menu = |cx: &mut TestAppContext, cell: &'static str, item: &'static str| {
        cx.update_window(o.win, |_, w, cx| w.right_click(cell, cx))
            .unwrap();
        cx.run_until_parked();
        cx.update_window(o.win, |_, w, cx| w.click(item, cx))
            .unwrap();
        cx.run_until_parked();
    };
    menu(cx, "cell-0-1", "grid-menu-filter-eq");
    applied(cx, "id > 0 AND name = 'b'", 1);
    let text = o.shell.read_with(cx, |s, cx| {
        let pane = s.tab().unwrap().read(cx).results_pane().read(cx);
        pane.filter_input().read(cx).value().to_string()
    });
    assert_eq!(text, "id > 0 AND name = 'b'");
    assert!(m.update(cx, |m, cx| m.stage_cell(0, 2, "z", cx)));
    cx.run_until_parked();
    menu(cx, "cell-0-2", "grid-menu-filter-null");
    assert!(present(cx, o.win, "confirm-dialog"));
    let q = m
        .read_with(cx, |m, _| m.discard_question().cloned())
        .unwrap();
    assert_eq!(
        q.action,
        Discard::Filter("id > 0 AND name = 'b' AND note IS NULL".into())
    );
}

/// Applies `clause` to the browse and returns the ids of the rows it keeps.
fn kept_ids(
    cx: &mut TestAppContext,
    m: &gpui_kit::Entity<dbento_app::tab_model::TabModel>,
    clause: &str,
) -> Vec<String> {
    assert!(
        m.update(cx, |m, cx| m.apply_filter(clause, cx)),
        "the filter applies"
    );
    wait_real(cx, clause, |cx| {
        m.read_with(cx, |m, _| {
            !m.running()
                && m.browse()
                    .is_some_and(|b| b.browse().filter.as_deref() == Some(clause))
        })
    });
    m.read_with(cx, |m, _| {
        let store = &m.results()[0].store;
        (0..store.rows())
            .map(|r| dbento_app::copy::cell_copy(store.cell(r, 0).unwrap()))
            .collect()
    })
}

#[gpui_kit::test]
fn a_sqlite_cell_filter_follows_the_cell_storage_class(cx: &mut TestAppContext) {
    let fx = fixture(
        "CREATE TABLE u(id INTEGER PRIMARY KEY, v, n NUMERIC); \
         INSERT INTO u VALUES (1, 1, 2), (2, '1', 2.5), (3, X'01ab', 'x'), (4, 'it''s', NULL);",
        "",
    );
    let m = ready(cx, &fx, "u");
    let clause = |cx: &mut TestAppContext, row, col| {
        m.read_with(cx, |m, _| {
            filter_clause(m, 0, GridTarget::Cell(row, col), "grid-menu-filter-eq")
        })
        .unwrap()
    };
    let want = [
        (0, 2, "v = 1", ["1"].as_slice()),
        (1, 2, "v = '1'", &["2"]),
        (2, 2, "v = X'01ab'", &["3"]),
        (3, 2, "v = 'it''s'", &["4"]),
        (0, 3, "n = 2", &["1"]),
        (1, 3, "n = 2.5", &["2"]),
        (2, 3, "n = 'x'", &["3"]),
    ];
    for (row, col, sql, _) in want {
        assert_eq!(clause(cx, row, col), sql);
    }
    for (_, _, sql, ids) in want {
        assert_eq!(kept_ids(cx, &m, sql), ids, "{sql}");
    }
}

#[gpui_kit::test]
fn a_postgres_cell_filter_follows_the_column_type(cx: &mut TestAppContext) {
    support::init(cx);
    support::init_tokio(cx);
    let rt = cx.update(|cx| dbento_app::tokio_bridge::handle(cx));
    let _enter = rt.enter();
    let (db, _dir, spec) = rt.block_on(support::pg_spec(""));
    rt.block_on(async {
        db.admin()
            .await
            .batch_execute(
                r#"CREATE TABLE p(id int PRIMARY KEY, n numeric, j json, pt point, "T" text, b bytea);
                   INSERT INTO p VALUES
                     (1, 1.50, '{"a": 1}', '(1,2)', 'x\y', '\x01ff'),
                     (2, 2, '[1]', '(3,4)', 'it''s', '\x02');"#,
            )
            .await
            .unwrap()
    });
    let m = cx.new(|_| dbento_app::tab_model::TabModel::new(spec));
    cx.update(|cx| {
        cx.subscribe(&m, |m, e: &dbento_app::browse::BrowseEvent, cx| {
            if let dbento_app::browse::BrowseEvent::Run(sql) = e {
                let sql = sql.clone();
                m.update(cx, |m, cx| m.run(sql, cx));
            }
        })
        .detach();
    });
    m.update(cx, |m, cx| {
        m.load_target(dbento_core::RelationRef::new("public", "p"), cx)
    });
    wait_real(cx, "the browse", |cx| {
        m.read_with(cx, |m, _| {
            !m.running() && m.results().first().is_some_and(|r| r.store.rows() == 2)
        })
    });
    let clause = |cx: &mut TestAppContext, row, col, id| {
        m.read_with(cx, |m, _| {
            filter_clause(m, 0, GridTarget::Cell(row, col), id)
        })
        .unwrap()
    };
    let eq = "grid-menu-filter-eq";
    let want = [
        (clause(cx, 0, 1, eq), "id = 1", ["1"].as_slice()),
        (clause(cx, 0, 2, eq), "n = 1.50", &["1"]),
        (clause(cx, 0, 3, eq), "j::text = '{\"a\": 1}'", &["1"]),
        (
            clause(cx, 1, 4, "grid-menu-filter-ne"),
            "pt::text <> '(3,4)'",
            &["1"],
        ),
        (clause(cx, 0, 5, eq), "\"T\" = E'x\\\\y'", &["1"]),
        (clause(cx, 1, 5, eq), "\"T\" = 'it''s'", &["2"]),
        (clause(cx, 1, 6, eq), "b = E'\\\\x02'", &["2"]),
        (clause(cx, 0, 3, "grid-menu-filter-null"), "j IS NULL", &[]),
    ];
    for (got, sql, ids) in want {
        assert_eq!(got, sql);
        assert_eq!(kept_ids(cx, &m, sql), ids, "{sql}");
    }
}

#[test]
fn and_filter_reads_sql_server_quoting() {
    let and = |e: &str| and_filter(Engine::Mssql, Some(e), "a = 1");
    assert_eq!(
        and("[x or y] = 1").as_deref(),
        Some("[x or y] = 1 AND a = 1")
    );
    assert_eq!(
        and("[x]] or y] = 1").as_deref(),
        Some("[x]] or y] = 1 AND a = 1")
    );
    assert_eq!(
        and("name = N'it''s or'").as_deref(),
        Some("name = N'it''s or' AND a = 1")
    );
    assert_eq!(
        and("/* a /* b */ or */ x = 1").as_deref(),
        Some("/* a /* b */ or */ x = 1 AND a = 1")
    );
    assert_eq!(
        and("x = 1 or y = 2 -- c").as_deref(),
        Some("a = 1 AND (x = 1 or y = 2) -- c")
    );
    assert_eq!(and("x = 1 /* a /* b */"), None);
    assert_eq!(and("[x = 1"), None);
    assert_eq!(and("x = N'open"), None);
}
