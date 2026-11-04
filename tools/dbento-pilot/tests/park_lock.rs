//! A commit in one tab is not blocked by a browse parked on the same SQLite file in another:
//! the commit lands and the other tab offers Fetch next as a continuation, under gpui-pilot.
mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use common::{Pilot, find_node, launch_args, sqlite_workspace};
use dbento_app::results_pane::continue_label;
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

async fn open_table(pilot: &Pilot, row: &str) {
    pilot
        .ok("click", json!({ "target": { "id": [row] }, "count": 2 }))
        .await;
}

async fn key(pilot: &Pilot, keys: &str) {
    pilot.ok("key", json!({ "keys": keys })).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_commit_frees_a_browse_parked_on_the_same_file_in_another_tab() {
    let ws = sqlite_workspace();
    exec(
        ws.path(),
        "CREATE TABLE big(id INTEGER PRIMARY KEY, v TEXT); \
         WITH RECURSIVE r(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM r WHERE x < 5000) \
         INSERT INTO big SELECT x, 'v' FROM r; \
         CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT); \
         INSERT INTO t VALUES (1, 'b'), (2, 'a'), (3, 'c');",
    )
    .await;
    let pilot = Pilot::start().await;
    pilot.ok("launch", launch_args("headless", ws.path())).await;
    click(&pilot, "connection-toggle-lite").await;
    until(&pilot, "the table rows", |t| {
        name(t, "tree-row-2") == Some("lite.main.big")
            && name(t, "tree-row-3") == Some("lite.main.t")
    })
    .await;

    // The big table's browse parks on its first rows, holding the file's read lock.
    open_table(&pilot, "tree-row-2").await;
    until(&pilot, "the big table's rows", |t| {
        find_node(t, "cell-0-1").is_some()
    })
    .await;
    open_table(&pilot, "tree-row-3").await;
    until(&pilot, "the second tab's three rows", |t| {
        find_node(t, "tab-1").is_some() && name(t, "footer-rows") == Some("3 rows")
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
    click(&pilot, "save-edits").await;
    until(&pilot, "the prompt", |t| {
        find_node(t, "apply-commit").is_some()
    })
    .await;
    click(&pilot, "apply-commit").await;
    let tree = until(&pilot, "the committed value", |t| {
        name(t, "cell-0-1") == Some("z") && find_node(t, "pending-changes").is_none()
    })
    .await;
    assert!(find_node(&tree, "edit-banner").is_none(), "{tree}");

    click(&pilot, "tab-0").await;
    let paused = continue_label(1_000);
    until(&pilot, "the continuation offer", |t| {
        name(t, "fetch-next") == Some(paused.as_str())
    })
    .await;
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    pilot.assert_bus_untouched();
}
