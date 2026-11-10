//! DynamoDB under gpui-pilot: the dialog, the tree, the badge, completion, documents and
//! the hidden SQL actions, against Moto.
mod common;
use std::path::Path;
use std::time::{Duration, Instant};

use common::{Pilot, find_node, launch_args};
use dbento_testkit::{DY_ACCESS_KEY, DY_SECRET, DY_SECRET_VAR, dynamo_db};
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
