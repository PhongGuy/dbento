//! A3 under gpui-pilot: Enter stages a record field with its hint; Stop hides when idle.
mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use common::{Pilot, find_node, launch_args, sqlite_workspace};
use serde_json::{Value, json};

/// Runs every statement of `sql` on the workspace's `lite` connection through core.
async fn exec(ws: &Path, sql: &str) {
    let w = dbento_core::Workspace::open(ws).unwrap();
    let conn = w.connection("lite").unwrap();
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

/// Polls the tree until `done` holds; returns that tree.
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

#[tokio::test(flavor = "multi_thread")]
async fn enter_stages_a_record_field_and_stop_waits_for_a_run() {
    let ws = sqlite_workspace();
    exec(
        ws.path(),
        "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT); INSERT INTO t VALUES (1, 'first');",
    )
    .await;
    let pilot = Pilot::start().await;
    pilot.ok("launch", launch_args("headless", ws.path())).await;
    // The lone schema `main` opens by itself (from A2), so the only table is row 2.
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
    let tree = until(&pilot, "one row", |t| {
        name(t, "footer-rows") == Some("1 row")
    })
    .await;
    // Idle after the run: the split Run button shows and Stop is gone.
    assert!(find_node(&tree, "run-split").is_some(), "{tree}");
    assert!(find_node(&tree, "cancel").is_none(), "{tree}");
    click(&pilot, "row-view-expand").await;
    click(&pilot, "record-value-1").await;
    until(&pilot, "the hint", |t| {
        name(t, "record-hint") == Some("Enter save · Shift+Enter new line · Esc cancel")
    })
    .await;
    pilot.ok("type", json!({ "text": "second" })).await;
    pilot.ok("key", json!({ "keys": "enter" })).await;
    until(&pilot, "one pending change", |t| {
        name(t, "pending-changes") == Some("1 pending change")
            && find_node(t, "record-editor").is_none()
    })
    .await;
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    pilot.assert_bus_untouched();
}
