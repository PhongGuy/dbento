//! A connection named `toggle-lite` beside one named `lite`: the row of the first and the
//! toggle of the second once both read `connection-toggle-lite`, so a click on that id could
//! land on the wrong element. A hyphenated name now shows bracketed, so the toggle is unique.
mod common;

use std::time::{Duration, Instant};

use common::{Pilot, find_node, launch_args};
use serde_json::{Value, json};

/// Two SQLite connections, `lite` with table `t` and `toggle-lite` with table `u`.
async fn two_connections() -> tempfile::TempDir {
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(
        ws.path().join("dbento.toml"),
        "dbento = 1\n\n[connections.lite]\nengine = \"sqlite\"\npath = \"app.db\"\n\n\
         [connections.toggle-lite]\nengine = \"sqlite\"\npath = \"other.db\"\n",
    )
    .unwrap();
    let w = dbento_core::Workspace::open(ws.path()).unwrap();
    for (name, file, sql) in [
        ("lite", "app.db", "CREATE TABLE t(id INTEGER PRIMARY KEY);"),
        (
            "toggle-lite",
            "other.db",
            "CREATE TABLE u(id INTEGER PRIMARY KEY);",
        ),
    ] {
        std::fs::write(ws.path().join(file), b"").unwrap();
        let conn = w.declared(name).unwrap();
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
    ws
}

fn names(tree: &Value) -> Vec<&str> {
    tree["nodes"]
        .as_array()
        .expect("tree nodes")
        .iter()
        .filter_map(|n| n["name"].as_str())
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_toggle_of_lite_is_not_confused_with_the_row_of_toggle_lite() {
    let ws = two_connections().await;
    let pilot = Pilot::start().await;
    pilot.ok("launch", launch_args("headless", ws.path())).await;
    let tree = pilot
        .ok(
            "tree",
            json!({ "wait_for": { "id": ["connection[toggle-lite]"] } }),
        )
        .await;
    assert!(find_node(&tree, "connection-lite").is_some(), "{tree}");

    pilot
        .ok(
            "click",
            json!({ "target": { "id": ["connection-toggle-lite"] } }),
        )
        .await;
    // Only `lite` expands: its table shows, and `toggle-lite` neither expands nor is selected.
    let deadline = Instant::now() + Duration::from_secs(30);
    let tree = loop {
        let tree = pilot.ok("tree", json!({})).await;
        if names(&tree).contains(&"lite.main.t") {
            break tree;
        }
        assert!(Instant::now() < deadline, "lite never expanded: {tree}");
        pilot.ok("wait", json!({ "ms": 50 })).await;
    };
    assert!(
        !names(&tree).iter().any(|n| n.starts_with("toggle-lite.")),
        "toggle-lite expanded too: {tree}"
    );
    let other = find_node(&tree, "connection[toggle-lite]").expect("toggle-lite's row");
    assert_ne!(
        other["selected"],
        json!(true),
        "toggle-lite was selected: {tree}"
    );
}
