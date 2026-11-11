//! The record view under gpui-pilot: toggle, step, edit a field, undo and redo it, save
//! and commit.
mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use common::{Pilot, find_node, launch_args, sqlite_workspace};
use serde_json::{Value, json};

/// Runs every statement of `sql` on the workspace's `lite` connection through core.
async fn exec(ws: &Path, sql: &str) {
    let w = dbento_core::Workspace::open(ws).unwrap();
    let conn = w.declared("lite").unwrap();
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

async fn click(pilot: &Pilot, id: &str) {
    pilot.ok("click", json!({ "target": { "id": [id] } })).await;
}

async fn key(pilot: &Pilot, keys: &str) {
    pilot.ok("key", json!({ "keys": keys })).await;
}

async fn shot(pilot: &Pilot) {
    let shot = pilot.call("screenshot", json!({})).await;
    assert!(shot.content.iter().any(|c| c.as_image().is_some()));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_record_is_stepped_edited_and_committed() {
    let ws = sqlite_workspace();
    exec(
        ws.path(),
        "CREATE TABLE a(id INTEGER PRIMARY KEY, name TEXT); \
         CREATE TABLE t(id INTEGER PRIMARY KEY, a_id INTEGER REFERENCES a(id), note TEXT); \
         INSERT INTO a VALUES (1, 'x'); INSERT INTO t VALUES (1, 1, 'first'), (2, 1, 'second');",
    )
    .await;
    let pilot = Pilot::start().await;
    pilot.ok("launch", launch_args("headless", ws.path())).await;
    click(&pilot, "connection-toggle-lite").await;
    pilot
        .ok("tree", json!({ "wait_for": { "id": ["tree-toggle-0"] } }))
        .await;
    until(&pilot, "the table row", |t| {
        name(t, "tree-row-3") == Some("lite.main.t")
    })
    .await;
    pilot
        .ok(
            "click",
            json!({ "target": { "id": ["tree-row-3"] }, "count": 2 }),
        )
        .await;
    until(&pilot, "two rows", |t| {
        name(t, "footer-rows") == Some("2 rows")
    })
    .await;
    let tree = pilot.ok("tree", json!({})).await;
    assert!(find_node(&tree, "result-pk-0").is_some(), "{tree}");
    assert_eq!(name(&tree, "result-fk-1"), Some("foreign key to a"));

    click(&pilot, "row-view-expand").await;
    until(&pilot, "the form", |t| {
        name(t, "record-position") == Some("Row 1 of 2")
    })
    .await;
    click(&pilot, "record-next").await;
    until(&pilot, "row 2", |t| {
        name(t, "record-value-2") == Some("second")
    })
    .await;
    shot(&pilot).await;
    click(&pilot, "record-value-2").await;
    until(&pilot, "the editor", |t| {
        find_node(t, "record-editor").is_some()
    })
    .await;
    pilot.ok("type", json!({ "text": "edited" })).await;
    key(&pilot, "ctrl-enter").await;
    until(&pilot, "one pending change", |t| {
        name(t, "pending-changes") == Some("1 pending change")
            && find_node(t, "record-modified-2").is_some()
    })
    .await;
    shot(&pilot).await;

    // Undo and redo reach the staged edit from the form, with no grid on screen.
    key(&pilot, "ctrl-z").await;
    until(&pilot, "the undo", |t| {
        name(t, "record-value-2") == Some("second") && find_node(t, "pending-changes").is_none()
    })
    .await;
    key(&pilot, "ctrl-shift-z").await;
    until(&pilot, "the redo", |t| {
        name(t, "pending-changes") == Some("1 pending change")
    })
    .await;

    // The grid's context menu (Add row, Set NULL) belongs to the grid alone.
    pilot
        .ok(
            "click",
            json!({ "target": { "id": ["record-name-1"] }, "button": "right" }),
        )
        .await;
    pilot.ok("wait", json!({ "ms": 300 })).await;
    let tree = pilot.ok("tree", json!({})).await;
    assert!(find_node(&tree, "grid-menu-add-row").is_none(), "{tree}");
    assert_eq!(name(&tree, "record-position"), Some("Row 2 of 2"));

    click(&pilot, "save-edits").await;
    until(&pilot, "the prompt", |t| {
        find_node(t, "apply-commit").is_some()
    })
    .await;
    click(&pilot, "apply-commit").await;
    until(&pilot, "the committed value", |t| {
        name(t, "record-value-2") == Some("edited") && find_node(t, "pending-changes").is_none()
    })
    .await;
    click(&pilot, "row-view-expand").await;
    until(&pilot, "the grid", |t| {
        find_node(t, "result-grid").is_some()
    })
    .await;
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    pilot.assert_bus_untouched();
}
