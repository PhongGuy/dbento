//! A conflict reload that finds the table's columns changed says so in the status bar and
//! leaves the staged edit unsaveable, under gpui-pilot.
mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use common::{Pilot, find_node, launch_args, sqlite_workspace};
use dbento_app::tab_model::COLUMNS_BLOCKED;
use serde_json::{Value, json};

/// Runs every statement of `sql` on the workspace's `lite` connection through core.
async fn exec(ws: &Path, sql: &str) {
    let w = dbento_core::Workspace::open(ws).unwrap();
    let conn = w.declared("lite").unwrap();
    let mut s = dbento_core::connect(&w, conn, None, Default::default())
        .await
        .unwrap();
    {
        let mut script = s.script(sql);
        while let Some(p) = script.next().await {
            let mut stream = p.unwrap().execute(Default::default()).await.unwrap();
            while stream.next_batch().await.unwrap().is_some() {}
        }
    }
    s.close().await.unwrap();
}

fn name<'a>(tree: &'a Value, id: &str) -> Option<&'a str> {
    find_node(tree, id).and_then(|n| n["name"].as_str())
}

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

async fn click(pilot: &Pilot, id: &str) {
    pilot.ok("click", json!({ "target": { "id": [id] } })).await;
}

async fn key(pilot: &Pilot, keys: &str) {
    pilot.ok("key", json!({ "keys": keys })).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_reload_over_changed_columns_says_so_and_stops_saving() {
    let ws = sqlite_workspace();
    exec(
        ws.path(),
        "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT); \
         INSERT INTO t VALUES (1, 'b'), (2, 'a'), (3, 'c');",
    )
    .await;
    let pilot = Pilot::start().await;
    pilot.ok("launch", launch_args("headless", ws.path())).await;
    click(&pilot, "connection-toggle-lite").await;
    until(&pilot, "the table row", |t| {
        name(t, "tree-row-2") == Some("lite.main.t")
    })
    .await;
    pilot
        .ok(
            "click",
            json!({ "target": { "id": ["tree-row-2"] }, "count": 2 }),
        )
        .await;
    until(&pilot, "three rows", |t| {
        name(t, "footer-rows") == Some("3 rows")
    })
    .await;

    click(&pilot, "cell-0-1").await;
    key(&pilot, "enter").await;
    until(&pilot, "the cell editor", |t| {
        find_node(t, "cell-editor").is_some()
    })
    .await;
    key(&pilot, "ctrl-a").await;
    pilot.ok("type", json!({ "text": "z" })).await;
    key(&pilot, "enter").await;

    exec(ws.path(), "DELETE FROM t WHERE id = 1").await;
    click(&pilot, "save-edits").await;
    until(&pilot, "the conflict actions", |t| {
        find_node(t, "edit-banner-reload").is_some()
    })
    .await;
    exec(
        ws.path(),
        "INSERT INTO t VALUES (1, 'q'); ALTER TABLE t RENAME COLUMN name TO label",
    )
    .await;
    click(&pilot, "edit-banner-reload").await;
    until(&pilot, "the column notice", |t| {
        name(t, "status-session") == Some(COLUMNS_BLOCKED)
    })
    .await;
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    pilot.assert_bus_untouched();
}
