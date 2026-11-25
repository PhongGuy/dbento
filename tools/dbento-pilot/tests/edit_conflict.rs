//! The lost-update repro under gpui-pilot: a save over a row changed elsewhere is refused,
//! a reload shows what the database holds now, and the next save goes through.
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

/// The first cell of `sql`'s first row, as text.
async fn scalar(ws: &Path, sql: &str) -> String {
    let w = dbento_core::Workspace::open(ws).unwrap();
    let conn = w.declared("lite").unwrap();
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

async fn save_until(pilot: &Pilot, what: &str, id: &str) {
    click(pilot, "save-edits").await;
    until(pilot, what, |t| find_node(t, id).is_some()).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_save_over_a_changed_row_conflicts_and_saves_after_a_reload() {
    let ws = sqlite_workspace();
    exec(
        ws.path(),
        "CREATE TABLE audit_log(at TEXT, detail TEXT); \
         INSERT INTO audit_log VALUES ('2026-11-24', 'original'), ('2026-11-25', 'other');",
    )
    .await;
    let pilot = Pilot::start().await;
    pilot.ok("launch", launch_args("headless", ws.path())).await;
    click(&pilot, "connection-toggle-lite").await;
    until(&pilot, "the table row", |t| {
        name(t, "tree-row-2") == Some("lite.main.audit_log")
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

    // The browse is `SELECT rowid, *`, so detail is data column 2 of rowid 1.
    click(&pilot, "cell-0-2").await;
    key(&pilot, "enter").await;
    until(&pilot, "the cell editor", |t| {
        find_node(t, "cell-editor").is_some()
    })
    .await;
    key(&pilot, "ctrl-a").await;
    pilot.ok("type", json!({ "text": "mine" })).await;
    key(&pilot, "enter").await;
    until(&pilot, "one pending change", |t| {
        name(t, "pending-changes") == Some("1 pending change")
    })
    .await;

    exec(
        ws.path(),
        "UPDATE audit_log SET detail = 'theirs' WHERE rowid = 1",
    )
    .await;
    save_until(&pilot, "the conflict banner", "edit-banner-reload").await;
    assert_eq!(
        scalar(ws.path(), "SELECT detail FROM audit_log WHERE rowid = 1").await,
        "theirs"
    );

    // The edited cell is still selected and a second click would select its whole row, so
    // the selection leaves and comes back to show the status bar's line for that cell.
    click(&pilot, "edit-banner-reload").await;
    click(&pilot, "cell-1-2").await;
    click(&pilot, "cell-0-2").await;
    until(&pilot, "the fresh value", |t| {
        name(t, "status-was") == Some("Was: original \u{b7} now in database: theirs")
    })
    .await;

    save_until(&pilot, "the prompt", "apply-commit").await;
    click(&pilot, "apply-commit").await;
    until(&pilot, "the commit", |t| {
        find_node(t, "pending-changes").is_none() && find_node(t, "apply-commit").is_none()
    })
    .await;
    assert_eq!(
        scalar(ws.path(), "SELECT detail FROM audit_log WHERE rowid = 1").await,
        "mine"
    );
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    pilot.assert_bus_untouched();
}
