//! uxfix B3 under gpui-pilot: the header and cell menus, the row view's expand, and a follow
//! into a preview tab with Back and Forward.
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

async fn click(pilot: &Pilot, id: &str, extra: Value) {
    let mut args = json!({ "target": { "id": [id] } });
    if let (Some(a), Some(e)) = (args.as_object_mut(), extra.as_object()) {
        a.extend(e.clone());
    }
    pilot.ok("click", args).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn header_and_cell_menus_expand_and_a_walked_preview() {
    let ws = sqlite_workspace();
    exec(
        ws.path(),
        "CREATE TABLE a(id INTEGER PRIMARY KEY, name TEXT); \
         CREATE TABLE b(id INTEGER PRIMARY KEY, a_id INTEGER REFERENCES a(id), note TEXT); \
         INSERT INTO a VALUES (1, 'x'), (2, 'y'); \
         INSERT INTO b VALUES (1, 1, 'n'), (2, 2, 'm'), (3, 1, 'o');",
    )
    .await;
    let pilot = Pilot::start().await;
    pilot.ok("launch", launch_args("headless", ws.path())).await;
    click(&pilot, "connection-toggle-lite", json!({})).await;
    pilot
        .ok("tree", json!({ "wait_for": { "id": ["tree-toggle-0"] } }))
        .await;
    until(&pilot, "the table row", |t| {
        name(t, "tree-row-3") == Some("lite.main.b")
    })
    .await;
    click(&pilot, "tree-row-3", json!({ "count": 2 })).await;
    until(&pilot, "three rows", |t| {
        name(t, "footer-rows") == Some("3 rows")
    })
    .await;

    click(&pilot, "result-col-2", json!({ "button": "right" })).await;
    until(&pilot, "the header menu", |t| {
        find_node(t, "header-menu-sort-desc").is_some()
    })
    .await;
    click(&pilot, "header-menu-sort-desc", json!({})).await;
    until(&pilot, "note descending", |t| {
        name(t, "cell-0-2") == Some("o")
    })
    .await;
    click(&pilot, "cell-0-2", json!({ "button": "right" })).await;
    until(&pilot, "the cell filters", |t| {
        find_node(t, "grid-menu-filter-eq").is_some()
    })
    .await;
    click(&pilot, "grid-menu-filter-eq", json!({})).await;
    until(&pilot, "one row", |t| {
        name(t, "footer-rows") == Some("1 row")
    })
    .await;

    click(&pilot, "inspector-toggle", json!({})).await;
    click(&pilot, "cell-0-1", json!({})).await;
    until(&pilot, "row 1", |t| {
        name(t, "inspector-title") == Some("Row 1")
    })
    .await;
    click(&pilot, "row-view-expand", json!({})).await;
    until(&pilot, "the expanded record", |t| {
        find_node(t, "record-view").is_some() && find_node(t, "inspector-panel").is_none()
    })
    .await;
    click(&pilot, "row-view-expand", json!({})).await;
    until(&pilot, "the dock back", |t| {
        find_node(t, "inspector-panel").is_some()
    })
    .await;

    click(&pilot, "ref-out-0", json!({})).await;
    until(&pilot, "a preview on a, its row selected", |t| {
        find_node(t, "tab-preview-1").is_some() && name(t, "inspector-title") == Some("Row 1")
    })
    .await;
    click(&pilot, "ref-in-0", json!({})).await;
    until(&pilot, "b by a_id in the preview", |t| {
        name(t, "footer-rows") == Some("2 rows") && find_node(t, "tab-2").is_none()
    })
    .await;
    click(&pilot, "inspector-back", json!({})).await;
    until(&pilot, "back on a", |t| name(t, "cell-0-1") == Some("x")).await;
    click(&pilot, "cell-0-1", json!({})).await;
    pilot.ok("key", json!({ "keys": "alt-right" })).await;
    until(&pilot, "forward on b", |t| {
        name(t, "footer-rows") == Some("2 rows")
    })
    .await;
    click(&pilot, "tab-1", json!({ "count": 2 })).await;
    until(&pilot, "pinned", |t| {
        find_node(t, "tab-preview-1").is_none()
    })
    .await;
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    pilot.assert_bus_untouched();
}
