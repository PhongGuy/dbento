//! uxfix B1 under gpui-pilot: the connection chip, the copy menu on a query result, the
//! editor's own menu and a failed query's body.
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

/// Polls the tree until `done` holds, failing with `what` after 30 seconds.
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

async fn click(pilot: &Pilot, id: &str, extra: Value) {
    let mut args = json!({ "target": { "id": [id] } });
    if let (Some(a), Some(e)) = (args.as_object_mut(), extra.as_object()) {
        a.extend(e.clone());
    }
    pilot.ok("click", args).await;
}

async fn key(pilot: &Pilot, keys: &str) {
    pilot.ok("key", json!({ "keys": keys })).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn chip_copy_menu_editor_menu_and_the_error_body() {
    let ws = sqlite_workspace();
    exec(
        ws.path(),
        "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT); \
         INSERT INTO t VALUES (1, 'a'), (2, 'b');",
    )
    .await;
    let pilot = Pilot::start().await;
    pilot.ok("launch", launch_args("headless", ws.path())).await;
    run_query(&pilot, "SELECT * FROM t", false).await;
    let tree = pilot.ok("tree", json!({})).await;
    assert_eq!(
        name(&tree, "query-connection"),
        Some("lite \u{b7} sqlite \u{b7} app.db")
    );

    click(&pilot, "cell-0-1", json!({ "button": "right" })).await;
    until(&pilot, "the copy menu", |t| {
        find_node(t, "grid-menu-copy-csv").is_some()
    })
    .await;
    key(&pilot, "escape").await;

    click(&pilot, "editor", json!({ "button": "right" })).await;
    until(&pilot, "the editor menu", |t| {
        find_node(t, "editor-menu-run").is_some()
    })
    .await;
    key(&pilot, "escape").await;

    click(&pilot, "editor", json!({})).await;
    key(&pilot, "ctrl-a").await;
    pilot
        .ok("type", json!({ "text": "SELECT nope_col FROM t" }))
        .await;
    click(&pilot, "run-all", json!({})).await;
    until(&pilot, "the failed body", |t| {
        name(t, "result-error") == Some("Query failed")
    })
    .await;
    assert!(find_node(&pilot.ok("tree", json!({})).await, "result-error-jump").is_some());
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    pilot.assert_bus_untouched();
}
