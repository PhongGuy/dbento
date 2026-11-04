//! Editing a browsed table under gpui-pilot: a cell edited, saved and committed; a delete
//! rolled back with Esc; a row deleted elsewhere marked on save.
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

/// The first cell of `sql`'s first row, as text.
async fn scalar(ws: &Path, sql: &str) -> String {
    let w = dbento_core::Workspace::open(ws).unwrap();
    let conn = w.connection("lite").unwrap();
    let mut s = dbento_core::connect(&w, conn, None, Default::default())
        .await
        .unwrap();
    let mut out = None;
    {
        let mut script = s.script(sql);
        let mut stream = script
            .next()
            .await
            .unwrap()
            .unwrap()
            .execute(Default::default())
            .await
            .unwrap();
        while let Some(b) = stream.next_batch().await.unwrap() {
            if out.is_none() && !b.is_empty() {
                out = Some(match b.cell(0, 0) {
                    dbento_core::CellRef::Text(t) => t.to_owned(),
                    other => format!("{other:?}"),
                });
            }
        }
    }
    s.close().await.unwrap();
    out.expect("a row")
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
async fn a_cell_is_committed_a_delete_rolled_back_and_a_conflict_marked() {
    let ws = sqlite_workspace();
    exec(
        ws.path(),
        "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT); \
         INSERT INTO t VALUES (1, 'b'), (2, 'a'), (3, 'c');",
    )
    .await;
    let pilot = Pilot::start().await;
    pilot.ok("launch", launch_args("headless", ws.path())).await;
    click(&pilot, "connection-toggle-lite").await;
    pilot
        .ok("tree", json!({ "wait_for": { "id": ["tree-toggle-0"] } }))
        .await;
    until(&pilot, "the table row", |t| {
        name(t, "tree-row-2") == Some("lite.main.t")
    })
    .await;
    pilot
        .ok(
            "click",
            json!({ "target": { "id": ["tree-row-2"] }, "count": 2 }),
        )
        .await;
    until(&pilot, "three rows", |t| {
        name(t, "footer-rows") == Some("3 rows")
    })
    .await;

    // Edit a cell from the keyboard, save, and commit.
    click(&pilot, "cell-0-1").await;
    key(&pilot, "enter").await;
    until(&pilot, "the cell editor", |t| {
        find_node(t, "cell-editor").is_some()
    })
    .await;
    pilot.ok("type", json!({ "text": "z" })).await;
    key(&pilot, "enter").await;
    until(&pilot, "one pending change", |t| {
        name(t, "pending-changes") == Some("1 pending change")
            && find_node(t, "cell-modified-0-1").is_some()
    })
    .await;
    shot(&pilot).await;
    click(&pilot, "save-edits").await;
    until(&pilot, "the prompt", |t| {
        find_node(t, "apply-commit").is_some()
    })
    .await;
    shot(&pilot).await;
    click(&pilot, "apply-commit").await;
    until(&pilot, "the committed value", |t| {
        name(t, "cell-0-1") == Some("z") && find_node(t, "pending-changes").is_none()
    })
    .await;
    assert_eq!(
        scalar(ws.path(), "SELECT name FROM t WHERE id = 1").await,
        "z"
    );

    // Delete a row and roll back with Esc; the delete stays staged.
    click(&pilot, "cell-1-1").await;
    key(&pilot, "delete").await;
    until(&pilot, "the deleted row", |t| {
        find_node(t, "row-deleted-1").is_some()
    })
    .await;
    click(&pilot, "save-edits").await;
    until(&pilot, "the prompt", |t| {
        find_node(t, "apply-rollback").is_some()
    })
    .await;
    key(&pilot, "escape").await;
    let tree = until(&pilot, "the roll back", |t| {
        find_node(t, "apply-rollback").is_none()
    })
    .await;
    assert!(find_node(&tree, "row-deleted-1").is_some(), "{tree}");
    assert_eq!(scalar(ws.path(), "SELECT count(*) FROM t").await, "3");
    shot(&pilot).await;
    click(&pilot, "revert-edits").await;
    until(&pilot, "the revert", |t| {
        find_node(t, "row-deleted-1").is_none()
    })
    .await;

    // A row deleted elsewhere is rejected and marked.
    click(&pilot, "cell-2-1").await;
    key(&pilot, "f2").await;
    until(&pilot, "the cell editor", |t| {
        find_node(t, "cell-editor").is_some()
    })
    .await;
    pilot.ok("type", json!({ "text": "q" })).await;
    key(&pilot, "enter").await;
    until(&pilot, "one pending change", |t| {
        find_node(t, "pending-changes").is_some()
    })
    .await;
    exec(ws.path(), "DELETE FROM t WHERE id = 3").await;
    click(&pilot, "save-edits").await;
    let tree = until(&pilot, "the banner", |t| {
        name(t, "edit-banner") == Some("Nothing was committed: 1 row was modified by someone else")
    })
    .await;
    assert_eq!(
        name(&tree, "row-failed-2"),
        Some("modified or deleted by someone else")
    );
    assert!(find_node(&tree, "apply-commit").is_none(), "{tree}");
    shot(&pilot).await;
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    pilot.assert_bus_untouched();
}
