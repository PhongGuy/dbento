//! A table that cannot open as a browse tab says why it ran as a plain query.
//! Set DBENTO_SHOTS_DIR to keep the shot.
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

async fn shot(pilot: &Pilot, name: &str) {
    if let Some(dir) = std::env::var_os("DBENTO_SHOTS_DIR").map(std::path::PathBuf::from) {
        std::fs::create_dir_all(&dir).unwrap();
        pilot
            .ok("screenshot", json!({ "save_to": dir.join(name) }))
            .await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_table_dropped_behind_the_explorer_says_why_it_cannot_browse() {
    let ws = sqlite_workspace();
    exec(ws.path(), "CREATE TABLE t(id INTEGER PRIMARY KEY);").await;
    let pilot = Pilot::start().await;
    pilot.ok("launch", launch_args("headless", ws.path())).await;
    pilot
        .ok(
            "click",
            json!({ "target": { "id": ["connection-toggle-lite"] } }),
        )
        .await;
    until(&pilot, "the table row", |t| {
        name(t, "tree-row-2") == Some("lite.main.t")
    })
    .await;
    // The explorer keeps listing the table it loaded, so opening it now cannot describe it.
    exec(ws.path(), "DROP TABLE t;").await;
    pilot
        .ok(
            "click",
            json!({ "target": { "id": ["tree-row-2"] }, "count": 2 }),
        )
        .await;
    until(&pilot, "the notice", |t| {
        name(t, "notice")
            == Some(
                "Opened as a plain query, so rows cannot be edited here: \
                 no table or view main.t",
            )
    })
    .await;
    shot(&pilot, "browse-why.png").await;
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    pilot.assert_bus_untouched();
}
