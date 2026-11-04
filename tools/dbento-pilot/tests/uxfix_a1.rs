//! uxfix A1 under gpui-pilot: a table's own menu, Count rows as a notice, and the
//! read-only note on a query result.
mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use common::{Pilot, find_node, launch_args, run_query, sqlite_workspace};
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

/// Clicks element `id`, merging `extra` (button, count) into the click arguments.
async fn click(pilot: &Pilot, id: &str, extra: Value) {
    let mut args = json!({ "target": { "id": [id] } });
    if let (Some(a), Some(e)) = (args.as_object_mut(), extra.as_object()) {
        a.extend(e.clone());
    }
    pilot.ok("click", args).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_table_menu_a_count_notice_and_the_read_only_note() {
    let ws = sqlite_workspace();
    exec(
        ws.path(),
        "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT); \
         INSERT INTO t VALUES (1, 'a'), (2, 'b');",
    )
    .await;
    let pilot = Pilot::start().await;
    pilot.ok("launch", launch_args("headless", ws.path())).await;
    click(&pilot, "connection-toggle-lite", json!({})).await;
    pilot
        .ok("tree", json!({ "wait_for": { "id": ["tree-toggle-0"] } }))
        .await;
    until(&pilot, "the table row", |t| {
        name(t, "tree-row-2") == Some("lite.main.t")
    })
    .await;

    click(&pilot, "tree-row-2", json!({ "button": "right" })).await;
    let tree = until(&pilot, "the table menu", |t| {
        find_node(t, "tree-menu-count").is_some()
    })
    .await;
    assert!(
        find_node(&tree, "menu-delete").is_none(),
        "no connection items"
    );
    click(&pilot, "tree-menu-count", json!({})).await;
    until(&pilot, "the count notice", |t| {
        name(t, "notice") == Some("main.t: 2 rows")
    })
    .await;

    run_query(&pilot, "SELECT * FROM t", false).await;
    click(&pilot, "cell-0-1", json!({ "count": 2 })).await;
    // The button waits on the explorer's catalog to say the relation is a table.
    let tree = until(&pilot, "the note and its button", |t| {
        find_node(t, "readonly-note").is_some() && find_node(t, "readonly-open").is_some()
    })
    .await;
    assert_eq!(name(&tree, "readonly-open"), Some("Open t to edit"));
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    pilot.assert_bus_untouched();
}
