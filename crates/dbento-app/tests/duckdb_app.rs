//! DuckDB in the app: the schema tree, a query, a table opened as a plain query, the
//! read-only refusal and the in-process mode conflict, on files the test builds.
#![cfg(feature = "duckdb")]
use crate::support;
use std::sync::Arc;

use crate::support::{
    Fx, Opened, click_id, label_of, open_connection_row, present, services, wait_real,
};
use dbento_app::explorer::{connection_id, part_id};
use dbento_app::query_tab::{READ_ONLY_DUCKDB_TOOLTIP, read_only_tooltip};
use dbento_app::result_body::{ErrorView, code_label};
use dbento_app::sidebar::{SidebarEvent, connection_tooltip};
use dbento_core::{Engine, ObjectRef, RelationKind, RelationRef, Workspace};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AnyWindowHandle, AppContext as _, TestAppContext};

const SETUP: &str = "CREATE TABLE t(id INTEGER PRIMARY KEY, l INTEGER[]); \
                     INSERT INTO t VALUES (1, [1, 2]), (2, NULL);";
const OPEN: [&str; 3] = ["duck.main", "duck.main/tables", "duck.main.t"];
const NOTE: &str = "Opened as a plain query, so rows cannot be edited here: \
                    editing rows is not available on duckdb yet";
const DUCKRO: &str =
    "[connections.duckro]\nengine = \"duckdb\"\npath = \"data/app.duckdb\"\nread_only = true\n";

/// The app on a workspace whose `duck` connection is DuckDB over `data/app.duckdb`, built by
/// `setup`, with `extra` lines in its table and `more` TOML after it; nothing opened.
fn duck(cx: &mut TestAppContext, setup: &str, extra: &str, more: &str) -> Opened {
    support::init(cx);
    support::init_tokio(cx);
    let (dir, files) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    std::fs::create_dir(dir.path().join("data")).unwrap();
    dbento_testkit::duckdb_file(&dir.path().join("data/app.duckdb"), setup);
    std::fs::write(
        dir.path().join("dbento.toml"),
        format!(
            "dbento = 1\n[connections.duck]\nengine = \"duckdb\"\npath = \"data/app.duckdb\"\n{extra}\n{more}"
        ),
    )
    .unwrap();
    let ws = Arc::new(Workspace::open(dir.path()).unwrap());
    let (win, shell) = cx
        .update(|cx| dbento_app::app::start(services(files.path()), Some(dir.path().into()), cx))
        .unwrap();
    present(cx, win, "connection-list");
    Opened {
        fx: Fx { dir, ws },
        files,
        win,
        shell,
    }
}
fn tree(cx: &mut TestAppContext, win: AnyWindowHandle) -> Vec<String> {
    (0usize..)
        .map_while(|i| label_of(cx, win, ("tree-row", i)))
        .collect()
}
fn rows_shown(cx: &mut TestAppContext, o: &Opened, n: &str) {
    wait_real(cx, n, |cx| {
        label_of(cx, o.win, "footer-rows").as_deref() == Some(n)
    });
}

#[gpui_kit::test]
fn the_tree_opens_main_and_lists_its_table(cx: &mut TestAppContext) {
    let o = duck(cx, SETUP, "", "");
    click_id(cx, o.win, part_id("connection-toggle", "duck"));
    wait_real(cx, "main's table", |cx| tree(cx, o.win) == OPEN);
}

#[gpui_kit::test]
fn a_query_shows_duckdb_values_and_the_path_chip(cx: &mut TestAppContext) {
    let o = duck(cx, SETUP, "", "");
    open_connection_row(cx, o.win, "duck");
    o.run(cx, "SELECT id, l FROM t ORDER BY id");
    o.idle(cx);
    rows_shown(cx, &o, "2 rows");
    assert_eq!(label_of(cx, o.win, "cell-0-1").as_deref(), Some("[1, 2]"));
    assert_eq!(
        label_of(cx, o.win, "query-connection").as_deref(),
        Some("duck \u{b7} duckdb \u{b7} data/app.duckdb")
    );
}

#[gpui_kit::test]
fn a_table_opens_as_a_plain_query_without_explain(cx: &mut TestAppContext) {
    let o = duck(cx, SETUP, "", "");
    o.open_relation(cx, "t", RelationKind::Table);
    wait_real(cx, "the plain-query note", |cx| {
        label_of(cx, o.win, "notice").as_deref() == Some(NOTE)
    });
    rows_shown(cx, &o, "2 rows");
    assert!(!present(cx, o.win, "explain") && !present(cx, o.win, "explain-tip"));
}

#[gpui_kit::test]
fn a_read_only_connection_refuses_a_write_in_the_database(cx: &mut TestAppContext) {
    let o = duck(cx, SETUP, "read_only = true", "");
    open_connection_row(cx, o.win, "duck");
    o.run(cx, "INSERT INTO t VALUES (3, NULL)");
    o.idle(cx);
    let says = label_of(cx, o.win, "result-error-message").unwrap_or_default();
    assert!(says.starts_with("read-only session:"), "{says}");
    o.run(cx, "SELECT count(*) FROM t");
    o.idle(cx);
    assert_eq!(label_of(cx, o.win, "cell-0-0").as_deref(), Some("2"));
}

#[gpui_kit::test]
fn a_second_connection_in_the_other_mode_names_the_conflict(cx: &mut TestAppContext) {
    let o = duck(cx, SETUP, "", DUCKRO);
    open_connection_row(cx, o.win, "duck");
    o.run(cx, "SELECT 1");
    o.idle(cx);
    open_connection_row(cx, o.win, "duckro");
    assert_eq!(o.shell.read_with(cx, |s, _| s.tabs().len()), 2);
    o.run(cx, "SELECT 1");
    o.idle(cx);
    wait_real(cx, "the refusal", |cx| {
        label_of(cx, o.win, "result-message")
            .is_some_and(|m| m.contains("is already open read-write in this process"))
    });
}

#[test]
fn the_duckdb_badge_names_what_read_only_closes() {
    assert_eq!(read_only_tooltip(Engine::Duckdb), READ_ONLY_DUCKDB_TOOLTIP);
    assert!(READ_ONLY_DUCKDB_TOOLTIP.contains("file access off"));
    assert!(READ_ONLY_DUCKDB_TOOLTIP.contains("read_csv"));
}

#[gpui_kit::test]
fn a_read_only_duckdb_tab_shows_the_duckdb_badge(cx: &mut TestAppContext) {
    let o = duck(cx, SETUP, "read_only = true", "");
    open_connection_row(cx, o.win, "duck");
    assert_eq!(
        label_of(cx, o.win, "read-only-badge").as_deref(),
        Some(READ_ONLY_DUCKDB_TOOLTIP)
    );
    assert_eq!(
        connection_tooltip(o.fx.ws.connection("duck").unwrap()),
        "duckdb \u{b7} data/app.duckdb \u{b7} Double-click to open a query tab"
    );
}

#[gpui_kit::test]
fn view_definition_on_a_duckdb_table_asks_for_it(cx: &mut TestAppContext) {
    let o = duck(cx, SETUP, "", "");
    click_id(cx, o.win, part_id("connection-toggle", "duck"));
    wait_real(cx, "main's table", |cx| tree(cx, o.win) == OPEN);
    let sidebar = o.shell.read_with(cx, |s, _| s.sidebar().cloned().unwrap());
    let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let out = seen.clone();
    cx.update(|cx| {
        cx.subscribe(&sidebar, move |_, e: &SidebarEvent, _| {
            if let SidebarEvent::ViewDefinition { connection, object } = e {
                out.borrow_mut().push((connection.clone(), object.clone()));
            }
        })
        .detach()
    });
    cx.update_window(o.win, |_, w, cx| w.right_click(("tree-row", 2usize), cx))
        .unwrap();
    cx.run_until_parked();
    click_id(cx, o.win, "tree-menu-view-definition");
    assert_eq!(
        *seen.borrow(),
        [(
            "duck".to_owned(),
            ObjectRef::relation(RelationRef::new("main", "t"))
        )]
    );
}

#[test]
fn a_duckdb_code_reads_as_its_error_class() {
    let v = ErrorView {
        message: "m".into(),
        code: Some("Invalid Input".into()),
        sqlstate: None,
        detail: None,
        hint: None,
        position: None,
        sql: "SELECT 1".into(),
    };
    assert_eq!(
        code_label(Some(Engine::Duckdb), &v).as_deref(),
        Some("Invalid Input Error")
    );
}

#[gpui_kit::test]
fn a_failed_duckdb_statement_shows_its_class(cx: &mut TestAppContext) {
    let o = duck(cx, SETUP, "", "");
    open_connection_row(cx, o.win, "duck");
    o.run(cx, "SELECT * FROM nope");
    o.idle(cx);
    assert_eq!(
        label_of(cx, o.win, "result-error-code").as_deref(),
        Some("Catalog Error")
    );
    let says = label_of(cx, o.win, "result-error-message").unwrap_or_default();
    assert!(
        says.contains("nope") && !says.starts_with("Catalog Error"),
        "{says}"
    );
}

#[gpui_kit::test]
fn a_use_shows_the_database_beside_the_connection_until_the_session_ends(cx: &mut TestAppContext) {
    let o = duck(cx, SETUP, "", "");
    open_connection_row(cx, o.win, "duck");
    o.run(cx, "SELECT 1");
    o.idle(cx);
    assert!(
        !present(cx, o.win, "status-database"),
        "on the configured one"
    );
    o.run(cx, "ATTACH ':memory:' AS side; USE side");
    o.idle(cx);
    wait_real(cx, "the database chip", |cx| {
        label_of(cx, o.win, "status-database").as_deref() == Some("db: side")
    });
    cx.update_window(o.win, |_, w, cx| w.right_click(connection_id("duck"), cx))
        .unwrap();
    cx.run_until_parked();
    click_id(cx, o.win, "menu-disconnect");
    assert!(
        !present(cx, o.win, "status-database"),
        "gone with the session"
    );
}
