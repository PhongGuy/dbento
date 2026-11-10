//! DynamoDB under gpui-pilot: the dialog, the tree, the badge, completion, documents and
//! the hidden SQL actions, against Moto.
mod common;
use std::path::Path;
use std::time::{Duration, Instant};

use common::{Pilot, bounds, find_node, launch_args};
use dbento_testkit::{DY_ACCESS_KEY, DY_SECRET, DY_SECRET_VAR, DynamoDb, dynamo_db};
use serde_json::{Value, json};

// Helpers copied from tests/redis.rs; `launch` passes the Moto secret as DY_SECRET_VAR.
async fn until(pilot: &Pilot, what: &str, done: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let tree = pilot.ok("tree", json!({})).await;
        if done(&tree) {
            return tree;
        }
        assert!(Instant::now() < deadline, "{what} never happened: {tree}");
        pilot.ok("wait", json!({ "ms": 50 })).await;
    }
}
fn name<'a>(tree: &'a Value, id: &str) -> Option<&'a str> {
    find_node(tree, id).and_then(|n| n["name"].as_str())
}
/// Each rendered tree row as its index and label. Row ids carry the absolute row index, which
/// passes 500 once a folder of 1,200 keys is open, so every node is scanned rather than a range.
fn rows(tree: &Value) -> impl Iterator<Item = (usize, Option<&str>)> {
    tree["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|n| {
            let id = n["id"]
                .as_str()
                .or_else(|| n["path"].as_array()?.last()?.as_str())?;
            let ix = id.strip_prefix("tree-row-")?.parse().ok()?;
            Some((ix, n["name"].as_str()))
        })
}
fn row_named(tree: &Value, aria: &str) -> Option<usize> {
    rows(tree).find_map(|(ix, label)| (label == Some(aria)).then_some(ix))
}
/// The lowest row index the explorer renders, which moves only while a scroll moves the list.
fn first_row(tree: &Value) -> Option<usize> {
    rows(tree).map(|(ix, _)| ix).min()
}
/// Scrolls the explorer until the row labelled `aria` lies wholly inside the list. An unseen
/// row is sought downward first, then upward once a scroll stops moving the list.
async fn seek(pilot: &Pilot, aria: &str) -> (Value, usize) {
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut sweep = 2;
    let mut last = None;
    loop {
        let tree = pilot.ok("tree", json!({})).await;
        assert!(Instant::now() < deadline, "row {aria} never showed: {tree}");
        if find_node(&tree, "schema-tree").is_none() {
            pilot.ok("wait", json!({ "ms": 50 })).await;
            continue;
        }
        let list = bounds(&tree, "schema-tree");
        let notches = match row_named(&tree, aria) {
            None => {
                let first = first_row(&tree);
                if last.is_some() && first == last {
                    sweep = -sweep;
                }
                last = first;
                sweep
            }
            Some(ix) => {
                let row = bounds(&tree, &format!("tree-row-{ix}"));
                if row.y < list.y - 0.5 {
                    -1
                } else if row.y + row.h > list.y + list.h + 0.5 {
                    1
                } else {
                    return (tree, ix);
                }
            }
        };
        let at = json!({ "x": list.x + list.w / 2.0, "y": list.y + list.h / 2.0 });
        pilot
            .ok(
                "scroll",
                json!({ "target": at, "notches": notches, "notch_ms": 1, "settle_ms": 50 }),
            )
            .await;
    }
}
async fn click(pilot: &Pilot, id: &str, extra: Value) {
    let mut args = json!({ "target": { "id": [id] } });
    if let (Some(a), Some(e)) = (args.as_object_mut(), extra.as_object()) {
        a.extend(e.clone());
    }
    pilot.ok("click", args).await;
}
/// Picks the segmented option labelled `label`.
async fn option(pilot: &Pilot, label: &str) {
    pilot
        .ok(
            "click",
            json!({ "target": { "role": "Tab", "name": label } }),
        )
        .await;
}
async fn fill(pilot: &Pilot, id: &str, text: &str) {
    pilot
        .ok("type", json!({ "target": { "id": [id] }, "text": text }))
        .await;
}

async fn launch(ws: &Path) -> Pilot {
    let pilot = Pilot::start().await;
    let mut args = launch_args("headless", ws);
    args["env"] = json!({ DY_SECRET_VAR: DY_SECRET });
    pilot.ok("launch", args).await;
    pilot
}

/// Clicks `part` (`tree-row`, `tree-toggle`) of the row named `aria`, with `extra`.
async fn row(pilot: &Pilot, aria: &str, part: &str, extra: Value) {
    let (_, ix) = seek(pilot, aria).await;
    click(pilot, &format!("{part}-{ix}"), extra).await;
}

/// A workspace whose connection `dy` is `db` with `extra` keys.
fn workspace(db: &DynamoDb, extra: &str) -> tempfile::TempDir {
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(
        ws.path().join("dbento.toml"),
        format!("dbento = 1\n{}", db.connection_toml("dy", extra)),
    )
    .unwrap();
    ws
}

#[tokio::test(flavor = "multi_thread")]
async fn a_dynamodb_connection_made_in_the_dialog_connects_and_saves() {
    let db = dynamo_db().await;
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("dbento.toml"), "dbento = 1\n").unwrap();
    let pilot = launch(ws.path()).await;
    click(&pilot, "connection-new", json!({})).await;
    until(&pilot, "the dialog", |t| {
        find_node(t, "conn-name").is_some()
    })
    .await;
    option(&pilot, "DynamoDB").await;
    until(&pilot, "the DynamoDB form", |t| {
        find_node(t, "conn-region").is_some()
    })
    .await;
    fill(&pilot, "conn-name", "dy").await;
    fill(&pilot, "conn-region", db.region()).await;
    fill(&pilot, "conn-endpoint", db.endpoint()).await;
    fill(&pilot, "conn-access-key-id", DY_ACCESS_KEY).await;
    option(&pilot, "Environment variable").await;
    fill(&pilot, "conn-env", DY_SECRET_VAR).await;
    click(&pilot, "conn-consistent-read", json!({})).await;
    click(&pilot, "conn-test", json!({})).await;
    let tree = until(&pilot, "the test", |t| {
        name(t, "conn-test-result").is_some_and(|r| r != "Testing\u{2026}")
    })
    .await;
    assert_eq!(name(&tree, "conn-test-result"), Some("Connected"));
    click(&pilot, "conn-save", json!({})).await;
    until(&pilot, "the dialog to close", |t| {
        find_node(t, "conn-dialog").is_none()
    })
    .await;
    let toml = std::fs::read_to_string(ws.path().join("dbento.toml")).unwrap();
    for want in [
        "engine = \"dynamodb\"",
        "consistent_read = true",
        DY_SECRET_VAR,
        db.region(),
    ] {
        assert!(toml.contains(want), "{want}: {toml}");
    }
    assert!(!toml.contains(DY_SECRET), "{toml}");
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    pilot.assert_bus_untouched();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_tree_opens_tables_and_indexes_and_keeps_sql_actions_hidden() {
    let db = dynamo_db().await;
    let ws = workspace(&db, "read_only = true");
    let pilot = launch(ws.path()).await;
    let r = db.region();
    click(&pilot, "connection-toggle-dy", json!({})).await;
    row(&pilot, &format!("dy.{r}.orders"), "tree-toggle", json!({})).await;
    until(&pilot, "the key columns and indexes", |t| {
        row_named(t, &format!("dy.{r}.orders.sk")).is_some()
            && row_named(t, &format!("dy.{r}.orders/indexes")).is_some()
    })
    .await;
    row(
        &pilot,
        &format!("dy.{r}.orders"),
        "tree-row",
        json!({ "button": "right" }),
    )
    .await;
    let tree = until(&pilot, "the table menu", |t| {
        find_node(t, "tree-menu-open").is_some()
    })
    .await;
    for id in [
        "tree-menu-count",
        "tree-menu-generate-insert",
        "tree-menu-copy-qualified",
        "tree-menu-view-definition",
    ] {
        assert!(find_node(&tree, id).is_none(), "{id}");
    }
    pilot.ok("key", json!({ "keys": "escape" })).await;

    // An index opens as its own PartiQL SELECT, which Moto answers with ResourceNotFound.
    row(
        &pilot,
        &format!("dy.{r}.orders/indexes"),
        "tree-toggle",
        json!({}),
    )
    .await;
    let index = format!("dy.{r}.orders/indexes.by_status");
    row(&pilot, &index, "tree-row", json!({ "button": "right" })).await;
    let tree = until(&pilot, "the index menu", |t| {
        find_node(t, "tree-menu-open").is_some()
    })
    .await;
    assert!(find_node(&tree, "tree-menu-view-definition").is_none());
    click(&pilot, "tree-menu-open", json!({})).await;
    until(&pilot, "the index error", |t| {
        find_node(t, "result-error-copy").is_some()
    })
    .await;
    click(&pilot, "result-error-copy", json!({})).await;
    let clip = pilot.ok("clipboard", json!({})).await;
    let text = clip["text"].as_str().unwrap_or_default();
    assert!(
        text.contains("SELECT * FROM \"orders\".\"by_status\""),
        "{text}"
    );
    assert!(text.contains("ResourceNotFoundException"), "{text}");

    // A table opens and runs its plain SELECT.
    row(
        &pilot,
        &format!("dy.{r}.orders"),
        "tree-row",
        json!({ "count": 2 }),
    )
    .await;
    until(&pilot, "the table's rows", |t| {
        name(t, "footer-rows").is_some_and(|n| n.starts_with(|c: char| c.is_ascii_digit()))
    })
    .await;
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
}
