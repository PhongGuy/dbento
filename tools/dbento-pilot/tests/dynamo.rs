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

async fn type_in(pilot: &Pilot, conn: &str, text: &str) {
    click(pilot, &format!("connection-{conn}"), json!({ "count": 2 })).await;
    pilot
        .ok("tree", json!({ "wait_for": { "id": ["editor"] } }))
        .await;
    pilot
        .ok(
            "type",
            json!({ "target": { "id": ["editor"] }, "text": text }),
        )
        .await;
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

#[tokio::test(flavor = "multi_thread")]
async fn sampled_attributes_are_marked_inferred() {
    let db = dynamo_db().await;
    let ws = workspace(&db, "read_only = true");
    let pilot = launch(ws.path()).await;
    let r = db.region();
    click(&pilot, "connection-toggle-dy", json!({})).await;
    row(&pilot, &format!("dy.{r}.orders"), "tree-toggle", json!({})).await;
    let (tree, ix) = seek(&pilot, &format!("dy.{r}.orders.status")).await;
    assert_eq!(
        name(&tree, &format!("tree-detail-{ix}")),
        Some("S · inferred")
    );
    let (tree, ix) = seek(&pilot, &format!("dy.{r}.orders.pk")).await;
    assert_eq!(name(&tree, &format!("tree-detail-{ix}")), Some("S"));
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_badge_words_best_effort_and_a_write_is_refused() {
    let db = dynamo_db().await;
    let ws = workspace(&db, "read_only = true");
    let pilot = launch(ws.path()).await;
    type_in(&pilot, "dy", "DELETE FROM \"h\" WHERE id = 'nope'").await;
    click(&pilot, "run-all", json!({})).await;
    let tree = until(&pilot, "the badge", |t| {
        name(t, "read-only-badge")
            .is_some_and(|b| b.starts_with("Read-only is best effort: DynamoDB"))
    })
    .await;
    assert!(name(&tree, "read-only-badge").unwrap().contains("IAM"));
    until(&pilot, "the refusal", |t| {
        find_node(t, "result-error-copy").is_some()
    })
    .await;
    click(&pilot, "result-error-copy", json!({})).await;
    let clip = pilot.ok("clipboard", json!({})).await;
    // Core's refusal text (dynamo/driver.rs, the read-only gate in execute).
    assert!(
        clip["text"]
            .as_str()
            .unwrap_or_default()
            .contains("this statement writes"),
        "{clip}"
    );
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
}

#[tokio::test(flavor = "multi_thread")]
async fn completion_inserts_a_quoted_table_name() {
    let db = dynamo_db().await;
    let ws = workspace(&db, "read_only = true");
    let pilot = launch(ws.path()).await;
    // Expanding the connection loads the table names completion reads.
    click(&pilot, "connection-toggle-dy", json!({})).await;
    until(&pilot, "the tables", |t| {
        row_named(t, &format!("dy.{}.orders", db.region())).is_some()
    })
    .await;
    type_in(&pilot, "dy", "SELECT * FROM ord").await;
    pilot.ok("wait", json!({ "ms": 500 })).await;
    pilot.ok("key", json!({ "keys": "enter" })).await;
    pilot.ok("key", json!({ "keys": "ctrl-a" })).await;
    pilot.ok("key", json!({ "keys": "ctrl-c" })).await;
    let clip = pilot.ok("clipboard", json!({})).await;
    let text = clip["text"].as_str().unwrap_or_default();
    assert_eq!(text.trim_end(), "SELECT * FROM \"orders\"", "{text}");
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
}

#[tokio::test(flavor = "multi_thread")]
async fn an_item_shows_as_a_document_and_copies_as_json_without_sql() {
    let db = dynamo_db().await;
    let ws = workspace(&db, "read_only = true");
    let pilot = launch(ws.path()).await;
    type_in(&pilot, "dy", "SELECT * FROM \"types\"").await;
    click(&pilot, "run-all", json!({})).await;
    let tree = until(&pilot, "one row", |t| {
        name(t, "footer-rows") == Some("1 row")
    })
    .await;
    assert!(
        find_node(&tree, "footer-capacity").is_none(),
        "Moto reports no capacity"
    );
    click(&pilot, "row-view-expand", json!({})).await;
    until(&pilot, "the mode row", |t| {
        find_node(t, "record-mode").is_some()
    })
    .await;
    option(&pilot, "Document").await;
    let tree = until(&pilot, "the document", |t| {
        find_node(t, "record-document").is_some()
    })
    .await;
    let shown = name(&tree, "record-document").unwrap_or_default();
    for want in [
        "12345678901234567890123456789012345678",
        "<<",
        "'x'",
        "'t1'",
    ] {
        assert!(shown.contains(want), "{want}: {shown}");
    }
    click(&pilot, "row-view-expand", json!({})).await;
    until(&pilot, "the grid", |t| find_node(t, "cell-0-1").is_some()).await;
    click(&pilot, "cell-0-1", json!({ "button": "right" })).await;
    let tree = until(&pilot, "the grid menu", |t| {
        find_node(t, "grid-menu-copy-json").is_some()
    })
    .await;
    assert!(
        find_node(&tree, "grid-menu-copy-sql").is_none(),
        "DynamoDB offers no Copy as SQL"
    );
    click(&pilot, "grid-menu-copy-json", json!({})).await;
    let clip = pilot.ok("clipboard", json!({})).await;
    let text = clip["text"].as_str().unwrap_or_default();
    assert!(
        text.contains("12345678901234567890123456789012345678"),
        "{text}"
    );
    assert!(
        text.contains("\"ss\":[\"x\",\"y\"]") || text.contains("\"ss\": [\"x\", \"y\"]"),
        "{text}"
    );
    assert_eq!(
        pilot.ok("close", json!({})).await["exit_code"],
        json!(0),
        "the app exits cleanly"
    );
    pilot.assert_bus_untouched();
}
