//! Alias completion under gpui-pilot: in the demo shop, `o.st` after `FROM orders o` offers
//! `status`.
//! The popup has no tree node, so Enter accepts it and the query runs only if `status` landed.
mod common;

use std::time::{Duration, Instant};

use common::{Pilot, demo_workspace, find_node};
use serde_json::{Value, json};

async fn until(pilot: &Pilot, what: &str, done: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let tree = pilot.ok("tree", json!({})).await;
        if done(&tree) {
            return tree;
        }
        assert!(Instant::now() < deadline, "{what} never happened: {tree}");
        pilot.ok("wait", json!({ "ms": 50 })).await;
    }
}

/// The index N of the tree row `tree-row-N` named `name`.
fn row_index(tree: &Value, name: &str) -> Option<String> {
    tree["nodes"].as_array()?.iter().find_map(|n| {
        let id = n["id"]
            .as_str()
            .or_else(|| n["path"].as_array()?.last()?.as_str())?;
        let ix = id.strip_prefix("tree-row-")?;
        (n["name"] == json!(name)).then(|| ix.to_owned())
    })
}

async fn click(pilot: &Pilot, id: &str, count: u32) {
    pilot
        .ok("click", json!({ "target": { "id": [id] }, "count": count }))
        .await;
}

fn name<'a>(tree: &'a Value, id: &str) -> Option<&'a str> {
    find_node(tree, id).and_then(|n| n["name"].as_str())
}

#[tokio::test(flavor = "multi_thread")]
async fn an_alias_completes_its_tables_columns() {
    let (_dir, ws) = demo_workspace().await;
    let state = tempfile::tempdir().unwrap();
    let pilot = Pilot::start().await;
    pilot
        .ok(
            "launch",
            json!({
                "mode": "headless", "build": false, "width": 1280, "height": 800, "scale": 1.0,
                "args": [&ws], "env": { "DBENTO_STATE_DIR": state.path() },
            }),
        )
        .await;

    // Expanding `orders` loads its columns into the catalog completion reads.
    click(&pilot, "connection-toggle-shop", 1).await;
    let tree = until(&pilot, "the orders row", |t| {
        row_index(t, "shop.main.orders").is_some()
    })
    .await;
    let orders = row_index(&tree, "shop.main.orders").unwrap();
    click(&pilot, &format!("tree-toggle-{orders}"), 1).await;
    until(&pilot, "the orders children", |t| {
        row_index(t, "shop.main.orders/indexes").is_some()
    })
    .await;
    click(&pilot, "connection-shop", 2).await;
    until(&pilot, "the editor", |t| find_node(t, "editor").is_some()).await;
    click(&pilot, "editor", 1).await;

    // Enter either accepts `status` or breaks the line after `o.st`, which SQLite rejects.
    pilot
        .ok(
            "type",
            json!({ "text": "SELECT * FROM orders o WHERE o.st" }),
        )
        .await;
    pilot.ok("wait", json!({ "ms": 500 })).await;
    pilot.ok("key", json!({ "keys": "enter" })).await;
    pilot.ok("type", json!({ "text": " = 'shipped'" })).await;
    click(&pilot, "run-all", 1).await;
    let tree = until(&pilot, "the result", |t| {
        name(t, "footer-rows").is_some_and(|s| s.ends_with("rows"))
            || find_node(t, "result-error").is_some()
    })
    .await;
    assert!(
        find_node(&tree, "result-error").is_none(),
        "`o.st` did not complete to `status`: {tree}"
    );
}
