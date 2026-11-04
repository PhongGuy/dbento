//! The README screenshots: the demo shop walked through a result grid, editor completion,
//! the explorer's definition dock, a staged edit's commit prompt and an MCP approval, dark
//! at 1280 x 800. `make screenshots` sets DBENTO_SHOTS_DIR to keep them in assets/screenshots.
mod common;

use std::time::{Duration, Instant};

use common::{Pilot, bounds, demo_workspace, find_node};
use dbento_app::app::AppPaths;
use dbento_core::{ApprovalRequest, ApprovalStore, Writes};
use serde_json::{Value, json};

/// The palette filter that finds the demo's saved top-customers query.
const TOP_CUSTOMERS: &str = "top-customers";
/// Typed on one line so no newline can accept a completion; a `c` typed after it opens the popup.
const COMPLETION_SQL: &str = "SELECT id, status, ordered_at FROM orders WHERE ";
const APPROVAL_SQL: &str =
    "UPDATE products SET price = round(price * 1.05, 2) WHERE category = 'Books'";

fn name<'a>(tree: &'a Value, id: &str) -> Option<&'a str> {
    find_node(tree, id).and_then(|n| n["name"].as_str())
}

/// The element id of the node named `name` whose id starts with `prefix`.
fn id_named(tree: &Value, prefix: &str, name: &str) -> Option<String> {
    tree["nodes"].as_array()?.iter().find_map(|n| {
        let id = n["id"]
            .as_str()
            .or_else(|| n["path"].as_array()?.last()?.as_str())?;
        (id.starts_with(prefix) && n["name"] == json!(name)).then(|| id.to_owned())
    })
}

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

/// Waits for the tree row named `row` and returns its index, the N of `tree-row-N`.
async fn row(pilot: &Pilot, row: &str) -> String {
    let tree = until(pilot, row, |t| id_named(t, "tree-row-", row).is_some()).await;
    let id = id_named(&tree, "tree-row-", row).unwrap();
    id["tree-row-".len()..].to_owned()
}

async fn click(pilot: &Pilot, id: &str, count: u32) {
    pilot
        .ok("click", json!({ "target": { "id": [id] }, "count": count }))
        .await;
}

async fn key(pilot: &Pilot, keys: &str) {
    pilot.ok("key", json!({ "keys": keys })).await;
}

/// Lets the frame settle, then takes the shot; kept as `file` when DBENTO_SHOTS_DIR is set.
async fn shot(pilot: &Pilot, file: &str) {
    pilot.ok("wait", json!({ "ms": 300 })).await;
    let mut args = json!({});
    if let Some(dir) = std::env::var_os("DBENTO_SHOTS_DIR").map(std::path::PathBuf::from) {
        std::fs::create_dir_all(&dir).unwrap();
        args["save_to"] = json!(dir.join(file));
    }
    let shot = pilot.call("screenshot", args).await;
    assert!(
        shot.content.iter().any(|c| c.as_image().is_some()),
        "{file}: no image"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_readme_screenshots() {
    let (_dir, ws) = demo_workspace().await;
    let state = tempfile::tempdir().unwrap();
    let pilot = Pilot::start().await;
    pilot
        .ok(
            "launch",
            json!({
                "mode": "headless", "build": false, "width": 1280, "height": 800, "scale": 1.0,
                "args": [&ws],
                "env": { "DBENTO_STATE_DIR": state.path(), dbento_pilot::THEME_VAR: "dark" },
            }),
        )
        .await;

    // Hero: the shop's tables in the explorer and a saved query's result in the grid.
    click(&pilot, "connection-toggle-shop", 1).await;
    row(&pilot, "shop.main.orders").await;
    click(&pilot, "connection-shop", 2).await;
    until(&pilot, "the editor", |t| find_node(t, "editor").is_some()).await;
    click(&pilot, "editor", 1).await;
    key(&pilot, "ctrl-o").await;
    until(&pilot, "the palette", |t| find_node(t, "palette").is_some()).await;
    pilot.ok("type", json!({ "text": TOP_CUSTOMERS })).await;
    key(&pilot, "enter").await;
    until(&pilot, "the saved query's tab", |t| {
        find_node(t, "palette").is_none() && id_named(t, "tab-", "top-customers.sql").is_some()
    })
    .await;
    click(&pilot, "run-all", 1).await;
    until(&pilot, "the result", |t| {
        name(t, "footer-rows") == Some("25 rows")
    })
    .await;
    shot(&pilot, "hero.png").await;

    // Completion: a second tab, with the popup offering `orders` columns, a table and
    // keywords. Expanding `orders` loads its columns into the catalog completion reads.
    let orders = row(&pilot, "shop.main.orders").await;
    click(&pilot, &format!("tree-toggle-{orders}"), 1).await;
    row(&pilot, "shop.main.orders/indexes").await;
    key(&pilot, "ctrl-t").await;
    until(&pilot, "a new tab", |t| {
        id_named(t, "tab-", "Query 2").is_some()
    })
    .await;
    click(&pilot, "editor", 1).await;
    pilot.ok("type", json!({ "text": COMPLETION_SQL })).await;
    // The popup has no tree node, so give it a moment to open on the last keystroke.
    pilot.ok("type", json!({ "text": "c" })).await;
    pilot.ok("wait", json!({ "ms": 500 })).await;
    shot(&pilot, "completion.png").await;
    key(&pilot, "escape").await;

    // Editing: two staged price changes on `products`, then the commit prompt.
    let products = row(&pilot, "shop.main.products").await;
    click(&pilot, &format!("tree-row-{products}"), 2).await;
    until(&pilot, "the products grid", |t| {
        find_node(t, "cell-0-4").is_some()
    })
    .await;
    for (cell, value) in [("cell-0-4", "24.5"), ("cell-1-4", "31")] {
        click(&pilot, cell, 1).await;
        key(&pilot, "enter").await;
        until(&pilot, "the cell editor", |t| {
            find_node(t, "cell-editor").is_some()
        })
        .await;
        pilot.ok("type", json!({ "text": value })).await;
        key(&pilot, "enter").await;
    }
    until(&pilot, "two pending changes", |t| {
        name(t, "pending-changes") == Some("2 pending changes")
    })
    .await;
    click(&pilot, "save-edits", 1).await;
    until(&pilot, "the commit prompt", |t| {
        find_node(t, "apply-commit").is_some()
    })
    .await;
    shot(&pilot, "editing.png").await;
    key(&pilot, "escape").await;
    until(&pilot, "the roll back", |t| {
        find_node(t, "apply-commit").is_none()
    })
    .await;
    click(&pilot, "revert-edits", 1).await;
    until(&pilot, "the revert", |t| {
        find_node(t, "pending-changes").is_none()
    })
    .await;

    // Explorer: an index of `orders` opened in the definition dock, left open, since closing
    // it after the drag hands its width to the explorer.
    let indexes = row(&pilot, "shop.main.orders/indexes").await;
    click(&pilot, &format!("tree-toggle-{indexes}"), 1).await;
    let index = row(&pilot, "shop.main.orders/indexes.orders_customer").await;
    click(&pilot, &format!("tree-row-{index}"), 1).await;
    key(&pilot, "enter").await;
    let tree = until(&pilot, "the definition", |t| {
        name(t, "definition-editor").is_some_and(|s| s.starts_with("CREATE INDEX orders_customer"))
    })
    .await;
    // The dock opens 320 px wide; widen it so the whole definition line shows.
    let dock = bounds(&tree, "definition-panel");
    let y = dock.y + dock.h / 2.0;
    pilot
        .ok(
            "drag",
            json!({ "from": { "x": dock.x - 3.0, "y": y }, "to": { "x": dock.x - 160.0, "y": y } }),
        )
        .await;
    shot(&pilot, "explorer.png").await;

    // MCP: a write an AI asked for, waiting on the approval dialog.
    let store = ApprovalStore::open(&AppPaths::in_dir(state.path()).approvals).unwrap();
    store
        .request(&ApprovalRequest::new(
            &ws,
            "shop",
            APPROVAL_SQL,
            Writes::Yes,
            vec![],
        ))
        .unwrap();
    until(&pilot, "the approval dialog", |t| {
        find_node(t, "approval-dialog").is_some()
    })
    .await;
    shot(&pilot, "approval.png").await;

    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    pilot.assert_bus_untouched();
}
