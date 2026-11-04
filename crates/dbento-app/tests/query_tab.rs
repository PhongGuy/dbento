mod support;
use dbento_app::query_tab::connection_chip_text;
use dbento_app::query_tab::{
    READ_ONLY_MSSQL_BEST_EFFORT_TOOLTIP, READ_ONLY_MSSQL_READER_TOOLTIP, READ_ONLY_MYSQL_TOOLTIP,
    READ_ONLY_PG_TOOLTIP, READ_ONLY_SQLITE_TOOLTIP, READ_ONLY_SURREAL_BEST_EFFORT_TOOLTIP,
    READ_ONLY_SURREAL_UNCONNECTED_TOOLTIP, READ_ONLY_SURREAL_VIEWER_TOOLTIP, confirm_title,
    read_only_badge_text, read_only_tooltip, read_only_tooltip_of,
};
use dbento_app::result_body::{ErrorView, code_label};
use dbento_core::{Engine, Unguarded};
use dbento_core::{ReadOnlyEnforcement, read_only_residual};
use gpui_kit::test::TestWindowExt as _;
use gpui_kit::{AppContext as _, TestAppContext, px};
use support::{LONG, label_of, opened, present, wait_real};
const T: &str = "CREATE TABLE t(x); INSERT INTO t VALUES (1),(2),(3);";

fn answer(cx: &mut TestAppContext, o: &support::Opened, id: &'static str) {
    wait_real(cx, "the dialog", |cx| present(cx, o.win, "confirm-dialog"));
    cx.update_window(o.win, |_, w, cx| w.click(id, cx)).unwrap();
    o.idle(cx);
}
#[gpui_kit::test]
fn clicking_a_connection_opens_its_tab(cx: &mut TestAppContext) {
    let o = opened(cx, T, "", 0);
    assert_eq!(
        label_of(cx, o.win, ("tab-connection", 0usize)).as_deref(),
        Some("lite")
    );
    for id in ["tab-bar", "editor", "run", "run-split", "result-grid"] {
        assert!(present(cx, o.win, id), "{id}");
    }
    assert!(!present(cx, o.win, "cancel"));
    assert!(!present(cx, o.win, "production-band") && !present(cx, o.win, "read-only-badge"));
}
#[gpui_kit::test]
fn run_executes_the_editor_text(cx: &mut TestAppContext) {
    let o = opened(cx, T, "", 0);
    o.run(cx, "SELECT x FROM t ORDER BY x");
    o.idle(cx);
    assert_eq!(label_of(cx, o.win, "cell-2-0").as_deref(), Some("3"));
}
#[gpui_kit::test]
fn an_unguarded_update_asks_and_skip_runs_nothing(cx: &mut TestAppContext) {
    let o = opened(cx, T, "", 0);
    o.run(cx, "UPDATE t SET x = 0");
    answer(cx, &o, "confirm-skip");
    assert_eq!(o.fx.count("SELECT count(*) FROM t WHERE x = 0"), 0);
    assert_eq!(
        label_of(cx, o.win, ("statement-result", 0usize)).as_deref(),
        Some("skipped")
    );
}
#[gpui_kit::test]
fn confirm_run_executes_it(cx: &mut TestAppContext) {
    let o = opened(cx, T, "", 0);
    o.run(cx, "UPDATE t SET x = 0");
    answer(cx, &o, "confirm-run");
    assert_eq!(o.fx.count("SELECT count(*) FROM t WHERE x = 0"), 3);
}
#[gpui_kit::test]
fn red_shows_the_band_the_tab_tag_and_read_only_the_badge(cx: &mut TestAppContext) {
    let o = opened(cx, T, "color = \"red\"\nread_only = true", 0);
    assert!(present(cx, o.win, "production-band"));
    assert_eq!(
        label_of(cx, o.win, ("tab-color", 0usize)).as_deref(),
        Some("red")
    );
    assert_eq!(
        label_of(cx, o.win, "read-only-badge").as_deref(),
        Some(READ_ONLY_SQLITE_TOOLTIP)
    );
}
#[test]
fn the_badge_tooltip_follows_the_engine() {
    assert_eq!(read_only_tooltip(Engine::Postgres), READ_ONLY_PG_TOOLTIP);
    assert_eq!(read_only_tooltip(Engine::Sqlite), READ_ONLY_SQLITE_TOOLTIP);
    assert!(READ_ONLY_PG_TOOLTIP.contains("SET lasts one statement"));
    assert_eq!(read_only_tooltip(Engine::Mysql), READ_ONLY_MYSQL_TOOLTIP);
    assert!(READ_ONLY_MYSQL_TOOLTIP.contains("only SELECT"));
}
#[gpui_kit::test]
fn cancel_stops_a_long_statement(cx: &mut TestAppContext) {
    let o = opened(cx, T, "", 0);
    o.run(cx, LONG);
    let m = o.model(cx);
    wait_real(cx, "the connection", |cx| {
        m.read_with(cx, |m, _| m.read_only().is_some())
    });
    std::thread::sleep(std::time::Duration::from_millis(300));
    cx.update_window(o.win, |_, w, cx| w.click("cancel", cx))
        .unwrap();
    o.idle(cx);
    let text = label_of(cx, o.win, ("statement-result", 0usize)).unwrap();
    assert!(text.contains("cancelled"), "{text}");
}
#[test]
fn the_dialog_names_the_kind_and_the_connection() {
    assert_eq!(
        confirm_title(Unguarded::UpdateWithoutWhere, "prod"),
        "UPDATE without WHERE on 'prod'"
    );
    assert_eq!(
        confirm_title(Unguarded::DeleteWithoutWhere, "prod"),
        "DELETE without WHERE on 'prod'"
    );
    assert_eq!(confirm_title(Unguarded::Drop, "prod"), "DROP on 'prod'");
    assert_eq!(
        confirm_title(Unguarded::Truncate, "prod"),
        "TRUNCATE on 'prod'"
    );
}
#[gpui_kit::test]
fn a_drop_asks_and_skip_keeps_the_table(cx: &mut TestAppContext) {
    let o = opened(cx, T, "", 0);
    o.run(cx, "DROP TABLE t");
    wait_real(cx, "the dialog", |cx| present(cx, o.win, "confirm-dialog"));
    assert_eq!(
        label_of(cx, o.win, "confirm-detail").as_deref(),
        Some("It removes the object and everything in it.")
    );
    answer(cx, &o, "confirm-skip");
    assert_eq!(o.fx.count("SELECT count(*) FROM t"), 3);
}
#[gpui_kit::test]
// The Cancel button goes through TabModel::cancel, which skips a statement still asking.
fn cancel_while_asking_skips_the_statement(cx: &mut TestAppContext) {
    let o = opened(cx, T, "", 0);
    o.run(cx, "UPDATE t SET x = 0");
    wait_real(cx, "the dialog", |cx| present(cx, o.win, "confirm-dialog"));
    let tab = o.shell.read_with(cx, |s, _| s.tab().unwrap().clone());
    cx.update(|cx| tab.update(cx, |t, cx| t.cancel(cx)));
    o.idle(cx);
    assert_eq!(o.fx.count("SELECT count(*) FROM t WHERE x = 0"), 0);
    assert_eq!(
        label_of(cx, o.win, ("statement-result", 0usize)).as_deref(),
        Some("skipped")
    );
    assert!(
        !present(cx, o.win, "confirm-dialog"),
        "the dialog stayed open"
    );
}
// Skip is both the default and the cancel choice, so neither key can run an unguarded write.
fn a_key_answers_skip(cx: &mut TestAppContext, key: &str) {
    let o = opened(cx, T, "", 0);
    o.run(cx, "UPDATE t SET x = 0");
    wait_real(cx, "the dialog", |cx| present(cx, o.win, "confirm-dialog"));
    cx.update_window(o.win, |_, w, cx| w.press(key, cx))
        .unwrap();
    o.idle(cx);
    assert_eq!(o.fx.count("SELECT count(*) FROM t WHERE x = 0"), 0);
    assert_eq!(
        label_of(cx, o.win, ("statement-result", 0usize)).as_deref(),
        Some("skipped")
    );
}
#[gpui_kit::test]
fn escape_answers_skip(cx: &mut TestAppContext) {
    a_key_answers_skip(cx, "escape");
}
#[gpui_kit::test]
fn enter_answers_skip(cx: &mut TestAppContext) {
    a_key_answers_skip(cx, "enter");
}
#[gpui_kit::test]
fn a_statement_waiting_on_its_question_says_so(cx: &mut TestAppContext) {
    let o = opened(cx, T, "", 0);
    o.run(cx, "UPDATE t SET x = 0");
    wait_real(cx, "the dialog", |cx| present(cx, o.win, "confirm-dialog"));
    assert_eq!(
        label_of(cx, o.win, ("statement-result", 0usize)).as_deref(),
        Some("waiting for confirmation")
    );
    answer(cx, &o, "confirm-skip");
}
#[gpui_kit::test]
fn a_red_folder_makes_its_connection_production_and_tints_tab_and_toolbar(cx: &mut TestAppContext) {
    let o = opened(
        cx,
        T,
        "folder = \"Prod/EU\"\n[folders.\"Prod\"]\ncolor = \"red\"",
        0,
    );
    assert!(present(cx, o.win, "production-band"));
    assert_eq!(
        label_of(cx, o.win, ("tab-color", 0usize)).as_deref(),
        Some("red")
    );
    assert_eq!(label_of(cx, o.win, "query-toolbar").as_deref(), Some("red"));
    assert_eq!(label_of(cx, o.win, "tab-accent").as_deref(), Some("red"));
}
#[gpui_kit::test]
fn an_untagged_connection_has_an_unlabelled_toolbar(cx: &mut TestAppContext) {
    let o = opened(cx, T, "", 0);
    assert!(present(cx, o.win, "query-toolbar"));
    assert_eq!(label_of(cx, o.win, "query-toolbar"), None);
    assert!(!present(cx, o.win, "tab-accent"));
}
#[gpui_kit::test]
fn production_is_a_chip_in_a_40_px_toolbar_not_a_band(cx: &mut TestAppContext) {
    let o = opened(cx, T, "color = \"red\"", 0);
    cx.update_window(o.win, |_, w, cx| {
        w.render_frame(cx);
        let (bar, chip) = (
            w.find("query-toolbar").bounds(),
            w.find("production-band").bounds(),
        );
        assert_eq!(bar.size.height, px(40.));
        assert_eq!(chip.size.height, px(22.));
        assert!(
            bar.contains(&chip.origin) && chip.bottom() <= bar.bottom(),
            "{chip:?} in {bar:?}"
        );
    })
    .unwrap();
    assert_eq!(
        label_of(cx, o.win, "production-band").as_deref(),
        Some("production")
    );
}
#[gpui_kit::test]
fn the_run_summary_counts_statements_after_a_run(cx: &mut TestAppContext) {
    let o = opened(cx, T, "", 0);
    assert!(!present(cx, o.win, "run-summary"));
    o.run(cx, "SELECT 1; SELECT 2");
    o.idle(cx);
    assert_eq!(
        label_of(cx, o.win, "run-summary").as_deref(),
        Some("2 statements")
    );
    o.run(cx, "SELECT 1");
    o.idle(cx);
    assert_eq!(
        label_of(cx, o.win, "run-summary").as_deref(),
        Some("1 statement")
    );
}

#[gpui_kit::test]
fn stop_appearing_does_not_move_its_neighbours(cx: &mut TestAppContext) {
    let o = opened(cx, T, "", 0);
    let spots = |cx: &mut TestAppContext| {
        cx.update_window(o.win, |_, w, cx| {
            w.render_frame(cx);
            ["query-save", "row-limit"].map(|id| w.find(id).bounds())
        })
        .unwrap()
    };
    let idle = spots(cx);
    o.run(cx, LONG);
    wait_real(cx, "the Stop button", |cx| present(cx, o.win, "cancel"));
    assert_eq!(spots(cx), idle);
    cx.update_window(o.win, |_, w, cx| w.click("cancel", cx))
        .unwrap();
    o.idle(cx);
}

fn ms_workspace() -> (tempfile::TempDir, dbento_core::Workspace) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("dbento.toml"),
        "dbento = 1\n\n[connections.ms]\nengine = \"mssql\"\nhost = \"db.example\"\n\
         database = \"shop\"\nuser = \"sa\"\nread_only = true\n\n[connections.rr]\n\
         engine = \"mssql\"\nhost = \"db.example\"\nuser = \"sa\"\nread_only = true\n\
         read_only_user = \"dbento_reader\"\n",
    )
    .unwrap();
    let ws = dbento_core::Workspace::open(dir.path()).unwrap();
    (dir, ws)
}

#[test]
fn the_sql_server_badge_names_its_enforcement_level() {
    let (_dir, ws) = ms_workspace();
    assert_eq!(
        read_only_tooltip_of(ws.connection("ms").unwrap()),
        READ_ONLY_MSSQL_BEST_EFFORT_TOOLTIP
    );
    assert_eq!(
        read_only_tooltip_of(ws.connection("rr").unwrap()),
        READ_ONLY_MSSQL_READER_TOOLTIP
    );
    assert_eq!(
        READ_ONLY_MSSQL_READER_TOOLTIP,
        "Read-only: runs as the read_only_user principal, so SQL Server refuses writes. \
         Procedures granted to public can still write."
    );
    assert_eq!(
        READ_ONLY_MSSQL_BEST_EFFORT_TOOLTIP,
        "Read-only is best effort: each batch is fenced in a rolled-back transaction and \
         parseable writes are refused, but a hidden commit can get past it. Set \
         read_only_user for enforcement."
    );
}

#[test]
fn the_sql_server_chip_names_its_database_and_its_errors_their_number() {
    let (_dir, ws) = ms_workspace();
    assert_eq!(
        connection_chip_text(ws.connection("ms").unwrap()),
        "ms \u{b7} mssql \u{b7} shop"
    );
    assert_eq!(
        connection_chip_text(ws.connection("rr").unwrap()),
        "rr \u{b7} mssql \u{b7} sa@db.example:1433"
    );
    let v = ErrorView {
        message: "Invalid object name 'nope'.".into(),
        code: Some("208".into()),
        sqlstate: None,
        detail: None,
        hint: None,
        position: None,
        sql: "SELECT * FROM nope".into(),
    };
    assert_eq!(
        code_label(Some(Engine::Mssql), &v).as_deref(),
        Some("error 208")
    );
}

fn sr_workspace() -> (tempfile::TempDir, dbento_core::Workspace) {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("dbento.toml"),
        "dbento = 1\n\n[connections.sr]\nengine = \"surrealdb\"\nhost = \"db.example\"\n\
         namespace = \"app\"\ndatabase = \"main\"\nread_only = true\n",
    )
    .unwrap();
    let ws = dbento_core::Workspace::open(dir.path()).unwrap();
    (dir, ws)
}

#[test]
fn the_surrealdb_badge_names_the_session_s_level_and_what_the_fence_cannot_undo() {
    let (_dir, ws) = sr_workspace();
    let c = ws.connection("sr").unwrap();
    let residual = read_only_residual(Engine::Surrealdb).unwrap();
    for (level, head) in [
        (
            Some(ReadOnlyEnforcement::Database),
            READ_ONLY_SURREAL_VIEWER_TOOLTIP,
        ),
        (
            Some(ReadOnlyEnforcement::BestEffort),
            READ_ONLY_SURREAL_BEST_EFFORT_TOOLTIP,
        ),
        (None, READ_ONLY_SURREAL_UNCONNECTED_TOOLTIP),
    ] {
        assert_eq!(read_only_badge_text(c, level), format!("{head} {residual}"));
    }
    assert_eq!(
        READ_ONLY_SURREAL_VIEWER_TOOLTIP,
        "Read-only: this VIEWER login cannot write, and Dbento runs each statement in a \
         transaction it cancels."
    );
    assert_eq!(
        READ_ONLY_SURREAL_BEST_EFFORT_TOOLTIP,
        "Read-only is best effort: Dbento runs each statement in a transaction it cancels and \
         refuses the writes it can see. Use a VIEWER login for enforcement."
    );
    let (_ms_dir, ms) = ms_workspace();
    assert_eq!(
        read_only_badge_text(
            ms.connection("ms").unwrap(),
            Some(ReadOnlyEnforcement::Database)
        ),
        READ_ONLY_MSSQL_BEST_EFFORT_TOOLTIP,
        "other engines read their config"
    );
}

#[test]
fn the_surrealdb_chip_names_namespace_and_database_and_its_errors_their_code() {
    let (_dir, ws) = sr_workspace();
    assert_eq!(
        connection_chip_text(ws.connection("sr").unwrap()),
        "sr \u{b7} surrealdb \u{b7} app/main"
    );
    let view = |code: &str| ErrorView {
        message: "The table 'nope' does not exist".into(),
        code: Some(code.into()),
        sqlstate: None,
        detail: None,
        hint: None,
        position: None,
        sql: "SELECT * FROM nope".into(),
    };
    assert_eq!(code_label(Some(Engine::Surrealdb), &view("0")), None);
    assert_eq!(
        code_label(Some(Engine::Surrealdb), &view("-32000")).as_deref(),
        Some("error -32000")
    );
}
