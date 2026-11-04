//! A table browsed under gpui-pilot: opened from the explorer, filtered, sorted by a header,
//! and restored by the next launch.
mod common;

use std::time::{Duration, Instant};

use common::{Pilot, find_node, launch_args, rows_loaded, sqlite_workspace};
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

/// Polls the tree until element `id` is named `want`; returns that tree.
async fn wait_name(pilot: &Pilot, id: &str, want: &str) -> Value {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let tree = pilot.ok("tree", json!({})).await;
        if find_node(&tree, id).and_then(|n| n["name"].as_str()) == Some(want) {
            return tree;
        }
        assert!(
            Instant::now() < deadline,
            "{id} never read {want:?}: {tree}"
        );
        pilot.ok("wait", json!({ "ms": 50 })).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_table_is_filtered_and_sorted_from_its_browse_tab() {
    let ws = seeded().await;
    let pilot = Pilot::start().await;
    pilot.ok("launch", launch_args("headless", ws.path())).await;
    pilot
        .ok(
            "click",
            json!({ "target": { "id": ["connection-toggle-lite"] } }),
        )
        .await;
    pilot
        .ok("tree", json!({ "wait_for": { "id": ["tree-toggle-0"] } }))
        .await;
    wait_name(&pilot, "tree-row-2", "lite.main.t").await;
    pilot
        .ok(
            "click",
            json!({ "target": { "id": ["tree-row-2"] }, "count": 2 }),
        )
        .await;
    let tree = wait_name(&pilot, "footer-rows", "3 rows").await;
    assert!(find_node(&tree, "browse-filter").is_some(), "{tree}");
    assert!(find_node(&tree, "browse-not-editable").is_none(), "{tree}");
    pilot
        .ok(
            "type",
            json!({ "target": { "id": ["browse-filter-input"] }, "text": "id > 1" }),
        )
        .await;
    pilot.ok("key", json!({ "keys": "enter" })).await;
    wait_name(&pilot, "footer-rows", "2 rows").await;
    pilot
        .ok("click", json!({ "target": { "id": ["result-col-1"] } }))
        .await;
    wait_name(&pilot, "result-sort-1", "ascending").await;
    wait_name(&pilot, "cell-0-1", "a").await;
    pilot
        .ok("click", json!({ "target": { "id": ["result-col-1"] } }))
        .await;
    wait_name(&pilot, "result-sort-1", "descending").await;
    let tree = wait_name(&pilot, "cell-0-1", "c").await;
    assert_eq!(rows_loaded(&tree), 2);
    let shot = pilot.call("screenshot", json!({})).await;
    assert!(shot.content.iter().any(|c| c.as_image().is_some()));
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    pilot.assert_bus_untouched();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_filtered_table_tab_comes_back_after_a_restart() {
    let ws = seeded().await;
    let pilot = Pilot::start().await;
    pilot.ok("launch", launch_args("headless", ws.path())).await;
    pilot
        .ok(
            "click",
            json!({ "target": { "id": ["connection-toggle-lite"] } }),
        )
        .await;
    wait_name(&pilot, "tree-row-2", "lite.main.t").await;
    pilot
        .ok(
            "click",
            json!({ "target": { "id": ["tree-row-2"] }, "count": 2 }),
        )
        .await;
    wait_name(&pilot, "footer-rows", "3 rows").await;
    pilot
        .ok(
            "type",
            json!({ "target": { "id": ["browse-filter-input"] }, "text": "id > 1" }),
        )
        .await;
    pilot.ok("key", json!({ "keys": "enter" })).await;
    wait_name(&pilot, "footer-rows", "2 rows").await;
    // A kill, not a window close: the session must already hold the filter it ran.
    pilot.ok("close", json!({ "keep_data": true })).await;

    // The next launch names the tab after its table and runs nothing until asked; the first
    // run browses again with the saved filter.
    let mut args = launch_args("headless", ws.path());
    args["keep_data"] = json!(true);
    let relaunched = pilot.ok("launch", args).await;
    assert_eq!(relaunched["reused"], json!(true), "{relaunched}");
    let tree = wait_name(&pilot, "tab-0", "main.t").await;
    assert!(find_node(&tree, "footer-rows").is_none(), "{tree}");
    pilot
        .ok("click", json!({ "target": { "id": ["run-all"] } }))
        .await;
    let tree = wait_name(&pilot, "footer-rows", "2 rows").await;
    assert!(find_node(&tree, "browse-filter").is_some(), "{tree}");
    assert!(find_node(&tree, "browse-not-editable").is_none(), "{tree}");
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    pilot.assert_bus_untouched();
}
