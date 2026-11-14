//! DuckDB in the app under gpui-pilot: the schema tree and a query on a workspace file, then
//! the lock message while another process holds the file read-write.
mod common;
use std::path::Path;
use std::time::{Duration, Instant};

use common::{Pilot, find_node, launch_args};
use serde_json::{Value, json};

const TOML: &str =
    "dbento = 1\n[connections.duck]\nengine = \"duckdb\"\npath = \"data/app.duckdb\"\n";
const SETUP: &str = "CREATE TABLE t(id INTEGER PRIMARY KEY, l INTEGER[]); \
                     INSERT INTO t VALUES (1, [1, 2]), (2, NULL)";

async fn until(pilot: &Pilot, what: &str, done: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let tree = pilot.ok("tree", json!({})).await;
        if done(&tree) {
            return tree;
        }
        assert!(Instant::now() < deadline, "{what} never happened: {tree}");
        pilot.ok("wait", json!({ "ms": 50 })).await;
    }
}
fn name<'a>(tree: &'a Value, id: &str) -> Option<&'a str> {
    find_node(tree, id).and_then(|n| n["name"].as_str())
}
async fn click(pilot: &Pilot, id: &str, extra: Value) {
    let mut args = json!({ "target": { "id": [id] } });
    if let (Some(a), Some(e)) = (args.as_object_mut(), extra.as_object()) {
        a.extend(e.clone());
    }
    pilot.ok("click", args).await;
}
/// A workspace whose `duck` connection is a DuckDB file built from `SETUP`.
fn workspace() -> tempfile::TempDir {
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("dbento.toml"), TOML).unwrap();
    std::fs::create_dir(ws.path().join("data")).unwrap();
    dbento_testkit::duckdb_file(&ws.path().join("data/app.duckdb"), SETUP);
    ws
}
async fn launch(ws: &Path) -> Pilot {
    let pilot = Pilot::start().await;
    pilot.ok("launch", launch_args("headless", ws)).await;
    pilot
}
/// Opens `duck`'s query tab, types `sql` and runs it.
async fn query(pilot: &Pilot, sql: &str) {
    click(pilot, "connection-duck", json!({ "count": 2 })).await;
    pilot
        .ok("tree", json!({ "wait_for": { "id": ["editor"] } }))
        .await;
    pilot
        .ok(
            "type",
            json!({ "target": { "id": ["editor"] }, "text": sql }),
        )
        .await;
    click(pilot, "run-all", json!({})).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn duckdb_shows_its_schema_tree_and_runs_a_query() {
    let ws = workspace();
    let pilot = launch(ws.path()).await;
    click(&pilot, "connection-toggle-duck", json!({})).await;
    let tree = until(&pilot, "the lone schema's table", |t| {
        name(t, "tree-row-2") == Some("duck.main.t")
    })
    .await;
    assert_eq!(name(&tree, "tree-row-0"), Some("duck.main"));
    assert_eq!(name(&tree, "tree-row-1"), Some("duck.main/tables"));
    query(&pilot, "SELECT id, l FROM t ORDER BY id").await;
    let tree = until(&pilot, "two rows", |t| {
        name(t, "footer-rows") == Some("2 rows")
    })
    .await;
    assert!(find_node(&tree, "result-error").is_none(), "{tree}");
    assert!(
        find_node(&tree, "tree-row-3").is_none(),
        "no objects row under main: {tree}"
    );
    assert!(
        find_node(&tree, "explain").is_none(),
        "Explain hides on DuckDB"
    );
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    pilot.assert_bus_untouched();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_file_held_read_write_by_another_process_says_so() {
    let ws = workspace();
    let w = dbento_core::Workspace::open(ws.path()).unwrap();
    let conn = w.declared("duck").unwrap();
    // This test process holds the file read-write, so the app is the second process.
    let held = dbento_core::connect(&w, conn, None, Default::default())
        .await
        .unwrap();
    let pilot = launch(ws.path()).await;
    query(&pilot, "SELECT 1").await;
    let tree = until(&pilot, "the lock message", |t| {
        name(t, "connect-error-message")
            .is_some_and(|m| m.contains("is open read-write in another process"))
    })
    .await;
    let says = name(&tree, "connect-error-message").unwrap_or_default();
    assert!(says.starts_with("cannot open the database:"), "{says}");
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    pilot.assert_bus_untouched();
    held.close().await.unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_use_shows_the_database_until_the_connection_closes() {
    let ws = workspace();
    let pilot = launch(ws.path()).await;
    query(&pilot, "ATTACH ':memory:' AS side; USE side").await;
    until(&pilot, "the database chip", |t| {
        name(t, "status-database") == Some("db: side")
    })
    .await;
    click(&pilot, "connection-duck", json!({ "button": "right" })).await;
    pilot
        .ok("tree", json!({ "wait_for": { "id": ["menu-disconnect"] } }))
        .await;
    click(&pilot, "menu-disconnect", json!({})).await;
    until(&pilot, "the chip gone with the session", |t| {
        find_node(t, "status-database").is_none()
    })
    .await;
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    pilot.assert_bus_untouched();
}
