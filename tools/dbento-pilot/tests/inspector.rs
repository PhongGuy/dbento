//! The row inspector under gpui-pilot: open it, read a row, follow a reference both ways.
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

async fn shot(pilot: &Pilot) {
    let shot = pilot.call("screenshot", json!({})).await;
    assert!(shot.content.iter().any(|c| c.as_image().is_some()));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_inspector_reads_a_row_and_follows_references() {
    let ws = sqlite_workspace();
    exec(
        ws.path(),
        "CREATE TABLE a(id INTEGER PRIMARY KEY, name TEXT); \
         CREATE TABLE b(id INTEGER PRIMARY KEY, a_id INTEGER REFERENCES a(id), note TEXT); \
         INSERT INTO a VALUES (1, 'x'), (2, 'y'); \
         INSERT INTO b VALUES (1, 1, 'n'), (2, 2, 'm'), (3, 1, 'o');",
    )
    .await;
    let pilot = Pilot::start().await;
    pilot.ok("launch", launch_args("headless", ws.path())).await;
    click(&pilot, "connection-toggle-lite").await;
    pilot
        .ok("tree", json!({ "wait_for": { "id": ["tree-toggle-0"] } }))
        .await;
    until(&pilot, "the table row", |t| {
        name(t, "tree-row-3") == Some("lite.main.b")
    })
    .await;
    pilot
        .ok(
            "click",
            json!({ "target": { "id": ["tree-row-3"] }, "count": 2 }),
        )
        .await;
    until(&pilot, "three rows", |t| {
        name(t, "footer-rows") == Some("3 rows")
    })
    .await;
    click(&pilot, "inspector-toggle").await;
    until(&pilot, "the inspector", |t| {
        find_node(t, "inspector-empty").is_some()
    })
    .await;
    click(&pilot, "cell-1-1").await;
    let tree = until(&pilot, "row 2", |t| {
        name(t, "inspector-title") == Some("Row 2")
    })
    .await;
    assert_eq!(name(&tree, "inspector-identity"), Some("id = 2"));
    assert_eq!(name(&tree, "ref-out-0"), Some("a id = 2"));
    shot(&pilot).await;
    click(&pilot, "ref-out-0").await;
    until(&pilot, "a filtered to id 2", |t| {
        name(t, "footer-rows") == Some("1 row") && name(t, "cell-0-1") == Some("y")
    })
    .await;
    click(&pilot, "cell-0-1").await;
    until(&pilot, "the incoming link", |t| {
        name(t, "ref-in-0") == Some("b a_id = 2")
    })
    .await;
    click(&pilot, "ref-in-0").await;
    until(&pilot, "b filtered to a_id 2", |t| {
        name(t, "footer-rows") == Some("1 row") && name(t, "cell-0-2") == Some("m")
    })
    .await;
    shot(&pilot).await;
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    pilot.assert_bus_untouched();
}
