//! Editing conveniences under gpui-pilot: Set NULL from the context menu, undo and redo,
//! an added row typed into, Copy as UPDATE on the clipboard, the discard question on a
//! filter and on reopening the table, and the insert committed.
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

async fn right_click(pilot: &Pilot, id: &str) {
    pilot
        .ok(
            "click",
            json!({ "target": { "id": [id] }, "button": "right" }),
        )
        .await;
}

async fn key(pilot: &Pilot, keys: &str) {
    pilot.ok("key", json!({ "keys": keys })).await;
}

async fn shot(pilot: &Pilot) {
    let shot = pilot.call("screenshot", json!({})).await;
    assert!(shot.content.iter().any(|c| c.as_image().is_some()));
}

#[tokio::test(flavor = "multi_thread")]
async fn menu_undo_insert_copy_and_the_discard_question() {
    let ws = sqlite_workspace();
    exec(
        ws.path(),
        "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT, note TEXT); \
         INSERT INTO t VALUES (1, 'b', 'x'), (2, 'a', 'y');",
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
    until(&pilot, "two rows", |t| {
        name(t, "footer-rows") == Some("2 rows")
    })
    .await;

    // Set NULL from the menu, then undo and redo it from the keyboard.
    right_click(&pilot, "cell-0-2").await;
    until(&pilot, "the menu", |t| {
        find_node(t, "grid-menu-set-null").is_some()
    })
    .await;
    shot(&pilot).await;
    click(&pilot, "grid-menu-set-null").await;
    until(&pilot, "the NULL", |t| {
        name(t, "cell-0-2") == Some("NULL")
            && name(t, "pending-changes") == Some("1 pending change")
    })
    .await;
    click(&pilot, "cell-1-1").await;
    key(&pilot, "ctrl-z").await;
    until(&pilot, "the undo", |t| {
        name(t, "cell-0-2") == Some("x") && find_node(t, "pending-changes").is_none()
    })
    .await;
    key(&pilot, "ctrl-shift-z").await;
    until(&pilot, "the redo", |t| name(t, "cell-0-2") == Some("NULL")).await;

    // Add a row from the menu and type into it.
    right_click(&pilot, "cell-1-1").await;
    until(&pilot, "the menu", |t| {
        find_node(t, "grid-menu-add-row").is_some()
    })
    .await;
    click(&pilot, "grid-menu-add-row").await;
    until(&pilot, "the inserted row", |t| {
        find_node(t, "row-inserted-0").is_some()
            && name(t, "pending-changes") == Some("2 pending changes")
    })
    .await;
    click(&pilot, "cell-0-1").await;
    key(&pilot, "enter").await;
    until(&pilot, "the cell editor", |t| {
        find_node(t, "cell-editor").is_some()
    })
    .await;
    pilot.ok("type", json!({ "text": "new" })).await;
    key(&pilot, "enter").await;
    until(&pilot, "the typed insert", |t| {
        name(t, "cell-0-1") == Some("new")
    })
    .await;
    shot(&pilot).await;

    // Copy loaded row 1 (table row 2) as UPDATE.
    right_click(&pilot, "cell-2-1").await;
    until(&pilot, "the menu", |t| {
        find_node(t, "grid-menu-copy-update").is_some()
    })
    .await;
    click(&pilot, "grid-menu-copy-update").await;
    let clip = pilot.ok("clipboard", json!({})).await;
    let text = clip["text"].as_str().unwrap_or_default().to_owned();
    assert!(
        text.starts_with("UPDATE ") && text.contains("WHERE"),
        "{clip}"
    );

    // A filter while edits are staged asks; Keep holds them.
    click(&pilot, "browse-filter-input").await;
    pilot.ok("type", json!({ "text": "id > 1" })).await;
    key(&pilot, "enter").await;
    until(&pilot, "the discard question", |t| {
        find_node(t, "discard-edits").is_some()
    })
    .await;
    shot(&pilot).await;
    click(&pilot, "discard-keep").await;
    until(&pilot, "the kept edits", |t| {
        find_node(t, "discard-edits").is_none()
            && name(t, "pending-changes") == Some("2 pending changes")
    })
    .await;

    // Reopening the browsed table from the tree re-runs it, so it asks too.
    pilot
        .ok(
            "click",
            json!({ "target": { "id": ["tree-row-2"] }, "count": 2 }),
        )
        .await;
    until(&pilot, "the reopen question", |t| {
        find_node(t, "discard-edits").is_some()
    })
    .await;
    click(&pilot, "discard-keep").await;
    until(&pilot, "the edits kept on reopen", |t| {
        find_node(t, "discard-edits").is_none()
            && name(t, "pending-changes") == Some("2 pending changes")
            && name(t, "cell-0-1") == Some("new")
    })
    .await;

    // Save and commit: the insert and the NULL land.
    click(&pilot, "save-edits").await;
    until(&pilot, "the prompt", |t| {
        find_node(t, "apply-commit").is_some()
    })
    .await;
    click(&pilot, "apply-commit").await;
    until(&pilot, "the committed browse", |t| {
        name(t, "footer-rows") == Some("3 rows") && find_node(t, "pending-changes").is_none()
    })
    .await;
    assert_eq!(scalar(ws.path(), "SELECT count(*) FROM t").await, "3");
    assert_eq!(
        scalar(
            ws.path(),
            "SELECT count(*) FROM t WHERE id = 1 AND note IS NULL"
        )
        .await,
        "1"
    );
    assert_eq!(
        scalar(ws.path(), "SELECT count(*) FROM t WHERE name = 'new'").await,
        "1"
    );
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    pilot.assert_bus_untouched();
}
