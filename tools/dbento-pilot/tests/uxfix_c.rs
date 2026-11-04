//! uxfix C under gpui-pilot: the title pill opens the palette, a table is found and opened,
//! and the commands mode lists Reload with its chip.
mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use common::{Pilot, find_node, launch_args, sqlite_workspace};
use serde_json::{Value, json};

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

/// The id of the rendered palette row named `label`.
fn row_named(tree: &Value, label: &str) -> Option<String> {
    (0..40)
        .map(|n| format!("palette-row-{n}"))
        .find(|id| name(tree, id) == Some(label))
}

#[tokio::test(flavor = "multi_thread")]
async fn the_palette_finds_a_table_and_lists_commands() {
    let ws = sqlite_workspace();
    exec(
        ws.path(),
        "CREATE TABLE orders(id INTEGER PRIMARY KEY); INSERT INTO orders VALUES (1);",
    )
    .await;
    let pilot = Pilot::start().await;
    pilot.ok("launch", launch_args("headless", ws.path())).await;
    // Nothing is open, so lite's tables wait behind its explicit "Search tables in" row.
    click(&pilot, "title-search").await;
    until(&pilot, "the palette", |t| find_node(t, "palette").is_some()).await;
    // A narrowing query keeps the last group, "Tables not loaded", inside the list height.
    pilot.ok("type", json!({ "text": "ord" })).await;
    let tree = until(&pilot, "the idle row", |t| {
        row_named(t, "Search tables in lite").is_some()
    })
    .await;
    click(&pilot, &row_named(&tree, "Search tables in lite").unwrap()).await;
    until(&pilot, "lite's tables", |t| {
        row_named(t, "main.orders \u{b7} lite").is_some()
    })
    .await;
    pilot.ok("key", json!({ "keys": "escape" })).await;
    pilot.ok("key", json!({ "keys": "escape" })).await;
    until(&pilot, "the close", |t| find_node(t, "palette").is_none()).await;

    // Reopened, the input has focus: type, and Enter opens the best match.
    click(&pilot, "title-search").await;
    until(&pilot, "the palette", |t| find_node(t, "palette").is_some()).await;
    pilot.ok("type", json!({ "text": "ord" })).await;
    until(&pilot, "orders first", |t| {
        name(t, "palette-row-0") == Some("main.orders \u{b7} lite")
    })
    .await;
    pilot.ok("key", json!({ "keys": "enter" })).await;
    until(&pilot, "the browse tab", |t| {
        find_node(t, "palette").is_none() && find_node(t, "tab-0").is_some()
    })
    .await;

    click(&pilot, "title-search").await;
    until(&pilot, "the palette again", |t| {
        find_node(t, "palette").is_some()
    })
    .await;
    pilot.ok("type", json!({ "text": "> reload" })).await;
    let tree = until(&pilot, "Reload", |t| {
        name(t, "palette-row-0") == Some("Reload dbento.toml")
    })
    .await;
    assert_eq!(name(&tree, "palette-keys-0"), Some("Ctrl Shift R"));
    pilot.ok("key", json!({ "keys": "escape" })).await;
    pilot.ok("key", json!({ "keys": "escape" })).await;
    until(&pilot, "the close", |t| find_node(t, "palette").is_none()).await;
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    pilot.assert_bus_untouched();
}
