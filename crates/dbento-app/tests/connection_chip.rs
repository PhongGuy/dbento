//! The connection chip leads the query toolbar, and its picker switches the tab's connection.
mod support;
use dbento_app::query_tab::connection_chip_text;
use dbento_app::tab_model::SwitchBlocked;
use dbento_core::RelationKind;
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, ElementId, TestAppContext};
use support::{LONG, Opened, fixture, label_of, launched, opened, present, wait_real};

const LITE: &str = "lite \u{b7} sqlite \u{b7} data/app.db";
const NOPG: &str = "nopg \u{b7} postgres \u{b7} x";

fn click(cx: &mut TestAppContext, o: &Opened, id: impl Into<ElementId>) {
    let id = id.into();
    cx.update_window(o.win, |_, w, cx| w.click(id, cx)).unwrap();
    cx.run_until_parked();
}
fn connection(cx: &mut TestAppContext, o: &Opened) -> String {
    o.model(cx)
        .read_with(cx, |m, _| m.spec().connection.clone())
}

#[test]
fn the_chip_names_the_connection_engine_and_target() {
    let fx = fixture("", "");
    assert_eq!(
        connection_chip_text(fx.ws.connection("lite").unwrap()),
        LITE
    );
    assert_eq!(
        connection_chip_text(fx.ws.connection("nopg").unwrap()),
        NOPG
    );
}

#[gpui_kit::test]
fn the_chip_and_badges_lead_the_toolbar(cx: &mut TestAppContext) {
    let o = opened(cx, "", "read_only = true", 0);
    assert_eq!(
        label_of(cx, o.win, "query-connection").as_deref(),
        Some(LITE)
    );
    cx.update_window(o.win, |_, w, cx| {
        w.render_frame(cx);
        let chip = w.find("query-connection").bounds();
        let badge = w.find("read-only-badge").bounds();
        let run = w.find("run").bounds();
        assert!(chip.right() <= badge.left(), "the badge follows the chip");
        assert!(badge.right() <= run.left(), "Run follows the badges");
    })
    .unwrap();
}

#[gpui_kit::test]
fn picking_a_connection_switches_and_keeps_the_sql_and_results(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    o.run(cx, "SELECT 1 AS a");
    o.idle(cx);
    click(cx, &o, "query-connection");
    let pick = |n: usize| ("query-connection-pick", n);
    assert_eq!(label_of(cx, o.win, pick(0)).as_deref(), Some("lite"));
    assert_eq!(label_of(cx, o.win, pick(1)).as_deref(), Some("nopg"));
    click(cx, &o, pick(1));
    assert_eq!(connection(cx, &o), "nopg");
    assert_eq!(
        label_of(cx, o.win, "query-connection").as_deref(),
        Some(NOPG)
    );
    let (sql, results) = o.shell.read_with(cx, |s, cx| {
        let t = s.tab().unwrap().read(cx);
        (t.sql(cx), t.model().read(cx).results().len())
    });
    assert_eq!((sql.as_str(), results), ("SELECT 1 AS a", 1));
}

#[gpui_kit::test]
fn picker_rows_carry_the_tag_dot_and_engine(cx: &mut TestAppContext) {
    let o = opened(cx, "", "color = \"teal\"", 0);
    click(cx, &o, "query-connection");
    let engine = |cx: &mut TestAppContext, n: usize| {
        label_of(cx, o.win, ("query-connection-pick-engine", n))
    };
    assert_eq!(engine(cx, 0).as_deref(), Some("sqlite"));
    assert_eq!(engine(cx, 1).as_deref(), Some("postgres"));
    assert!(present(cx, o.win, ("query-connection-pick-dot", 0usize)));
    assert!(
        !present(cx, o.win, ("query-connection-pick-dot", 1usize)),
        "an untagged connection has no dot"
    );
}

#[gpui_kit::test]
fn a_switch_cuts_a_running_statement_and_keeps_its_result(cx: &mut TestAppContext) {
    let o = opened(cx, "", "", 0);
    let m = o.model(cx);
    o.run(cx, LONG);
    wait_real(cx, "the statement to start", |cx| {
        m.read_with(cx, |m, _| m.running() && !m.results().is_empty())
    });
    let ws = m.read_with(cx, |m, _| m.spec().workspace.clone());
    let switched = m.update(cx, |m, cx| m.switch_connection("nopg", ws, cx));
    assert!(switched.is_ok());
    m.read_with(cx, |m, _| {
        assert!(!m.running(), "the run ends");
        assert_eq!(m.results().len(), 1, "its result stays until the next run");
        assert_eq!(m.spec().connection, "nopg");
    });
}

#[gpui_kit::test]
fn same_unknown_and_browsing_tabs_are_refused(cx: &mut TestAppContext) {
    let o = launched(cx, "CREATE TABLE t(id INTEGER PRIMARY KEY);", "");
    o.open_relation(cx, "t", RelationKind::Table);
    o.wait_browse(cx, "SELECT *\nFROM main.t\nORDER BY id ASC");
    let m = o.model(cx);
    let tried = |cx: &mut TestAppContext, name: &str| {
        let ws = m.read_with(cx, |m, _| m.spec().workspace.clone());
        m.update(cx, |m, cx| m.switch_connection(name, ws, cx).err())
    };
    assert_eq!(tried(cx, "lite"), Some(SwitchBlocked::Same));
    assert_eq!(tried(cx, "gone"), Some(SwitchBlocked::Unknown));
    assert_eq!(tried(cx, "nopg"), Some(SwitchBlocked::Browse));
    click(cx, &o, "query-connection");
    assert!(
        !present(cx, o.win, ("query-connection-pick", 0usize)),
        "static on a browse tab"
    );
}

// The picker row labelled `name`, if the open picker lists it.
fn pick_row(cx: &mut TestAppContext, o: &Opened, name: &str) -> Option<usize> {
    (0..4).find(|&n| label_of(cx, o.win, ("query-connection-pick", n)).as_deref() == Some(name))
}

#[gpui_kit::test]
fn a_switch_after_a_reload_opens_the_session_dbento_toml_now_describes(cx: &mut TestAppContext) {
    let two = "\n[connections.two]\nengine = \"sqlite\"\npath = \"data/app.db\"";
    let o = opened(cx, "", two, 0);
    support::rewrite(
        &o.fx,
        &format!("{two}\nread_only = true"),
        "\n[connections.three]\nengine = \"sqlite\"\npath = \"data/app.db\"",
    );
    support::reload(cx, o.win);
    click(cx, &o, "query-connection");
    assert!(
        pick_row(cx, &o, "three").is_some(),
        "the picker lists the reloaded connections"
    );
    let row = pick_row(cx, &o, "two").expect("two is listed");
    click(cx, &o, ("query-connection-pick", row));
    let m = o.model(cx);
    wait_real(cx, "the new session", |cx| {
        m.read_with(cx, |m, _| m.read_only().is_some())
    });
    assert_eq!(
        m.read_with(cx, |m, _| m.read_only()),
        Some(true),
        "the session is read-only"
    );
    assert!(present(cx, o.win, "read-only-badge"));
}

#[gpui_kit::test]
fn a_kept_sqlite_result_keeps_its_engine_after_a_switch_to_postgres(cx: &mut TestAppContext) {
    let o = opened(
        cx,
        r"CREATE TABLE t(c BLOB, s TEXT); INSERT INTO t VALUES (x'01ff', 'a\b');",
        "",
        0,
    );
    let m = o.model(cx);
    let switch = |cx: &mut TestAppContext, name: &str| {
        let ws = m.read_with(cx, |m, _| m.spec().workspace.clone());
        assert!(
            m.update(cx, |m, cx| m.switch_connection(name, ws, cx))
                .is_ok()
        );
        cx.run_until_parked();
    };
    o.run(cx, "SELECT * FROM t");
    o.idle(cx);
    switch(cx, "nopg");
    let pane = o
        .shell
        .read_with(cx, |s, cx| s.tab().unwrap().read(cx).results_pane().clone());
    pane.update(cx, |p, cx| {
        p.table().update(cx, |t, cx| {
            t.set_selection(gpui_kit::component::table::TableSelection::Row(0), cx)
        })
    });
    cx.update_window(o.win, |_, w, cx| {
        pane.update(cx, |p, cx| p.copy_selection_as_insert(w, cx))
    })
    .unwrap();
    cx.run_until_parked();
    assert_eq!(
        cx.read_from_clipboard().and_then(|c| c.text()).as_deref(),
        Some(r"INSERT INTO main.t (c, s) VALUES (X'01ff', 'a\b');")
    );
    switch(cx, "lite");
    o.run(cx, "SELECT nope FROM t");
    o.idle(cx);
    switch(cx, "nopg");
    assert_eq!(
        label_of(cx, o.win, "result-error-code").as_deref(),
        Some("code 1")
    );
}

#[test]
fn a_mysql_chip_names_its_database_or_its_server() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("dbento.toml"),
        "dbento = 1\n[connections.shop]\nengine = \"mysql\"\nhost = \"h\"\nuser = \"u\"\ndatabase = \"sales\"\n\
         [connections.all]\nengine = \"mysql\"\nhost = \"h\"\nuser = \"u\"\n",
    )
    .unwrap();
    let ws = dbento_core::Workspace::open(dir.path()).unwrap();
    assert_eq!(
        connection_chip_text(ws.connection("shop").unwrap()),
        "shop \u{b7} mysql \u{b7} sales"
    );
    assert_eq!(
        connection_chip_text(ws.connection("all").unwrap()),
        "all \u{b7} mysql \u{b7} u@h:3306"
    );
}
