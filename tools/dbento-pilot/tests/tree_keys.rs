//! The explorer tree driven by keys alone in the running app: once a click has focused it,
//! the arrows expand a connection, a schema and a table, and Enter opens the table.
mod common;

use std::time::{Duration, Instant};

use common::{Pilot, find_node, launch_args, sqlite_workspace};
use serde_json::{Value, json};

/// The empty `lite` workspace with table `t` holding three rows, written through core.
async fn seeded() -> tempfile::TempDir {
    let ws = sqlite_workspace();
    let w = dbento_core::Workspace::open(ws.path()).unwrap();
    let conn = w.connection("lite").unwrap();
    let mut s = dbento_core::connect(&w, conn, None, Default::default())
        .await
        .unwrap();
    {
        let mut script = s.script(
            "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT); \
             INSERT INTO t VALUES (1, 'b'), (2, 'a'), (3, 'c');",
        );
        while let Some(p) = script.next().await {
            let mut stream = p.unwrap().execute(Default::default()).await.unwrap();
            while stream.next_batch().await.unwrap().is_some() {}
        }
    }
    s.close().await.unwrap();
    ws
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

async fn key(pilot: &Pilot, keys: &str) {
    pilot.ok("key", json!({ "keys": keys })).await;
}

fn named(tree: &Value, id: &str) -> Option<String> {
    find_node(tree, id).and_then(|n| n["name"].as_str().map(str::to_owned))
}

fn is_selected(tree: &Value, id: &str) -> bool {
    find_node(tree, id).is_some_and(|n| n["selected"] == json!(true))
}

async fn selected(pilot: &Pilot, id: &str) -> Value {
    until(pilot, &format!("{id} selected"), |t| is_selected(t, id)).await
}

#[tokio::test(flavor = "multi_thread")]
async fn the_explorer_tree_is_walked_and_opened_by_keys() {
    let ws = seeded().await;
    let pilot = Pilot::start().await;
    pilot.ok("launch", launch_args("headless", ws.path())).await;
    // One click focuses the explorer on `lite`; from here on only keys touch the tree.
    pilot
        .ok("click", json!({ "target": { "id": ["connection-lite"] } }))
        .await;
    selected(&pilot, "connection-lite").await;

    // Right expands the connection; its lone schema opens itself.
    key(&pilot, "right").await;
    until(&pilot, "the connection expanded", |t| {
        named(t, "tree-row-2").as_deref() == Some("lite.main.t")
    })
    .await;
    // Down to the schema, Left collapses it and Right expands it again.
    key(&pilot, "down").await;
    selected(&pilot, "tree-row-0").await;
    key(&pilot, "left").await;
    until(&pilot, "the schema collapsed", |t| {
        find_node(t, "tree-row-1").is_none()
    })
    .await;
    key(&pilot, "right").await;
    until(&pilot, "the schema expanded", |t| {
        named(t, "tree-row-2").as_deref() == Some("lite.main.t")
    })
    .await;
    // Right enters the tables group, then the table, then loads its columns.
    key(&pilot, "right").await;
    selected(&pilot, "tree-row-1").await;
    key(&pilot, "right").await;
    selected(&pilot, "tree-row-2").await;
    key(&pilot, "right").await;
    until(&pilot, "the table's columns", |t| {
        named(t, "tree-row-3").as_deref() == Some("lite.main.t.id")
    })
    .await;
    assert!(
        find_node(&pilot.ok("tree", json!({})).await, "tab-0").is_none(),
        "nothing opened yet"
    );

    // Enter opens the selected table in a browse tab.
    key(&pilot, "enter").await;
    until(&pilot, "the table browsed", |t| {
        named(t, "footer-rows").as_deref() == Some("3 rows")
    })
    .await;
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    pilot.assert_bus_untouched();
}
