//! The join dialog under gpui-pilot: sign in on the loopback stub, pick the workspace, clone
//! it into a folder, and land in it synced.
mod common;

use common::team::{name, team_fixture_with, until};
use common::{Pilot, find_node};
use dbento_app::explorer::part_a11y_id;
use serde_json::json;

// As rename_secret.rs's `fill`: `type` with a target focuses that field first.
async fn type_into(pilot: &Pilot, id: &str, text: &str) {
    pilot
        .ok("type", json!({ "target": { "id": [id] }, "text": text }))
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn join_clones_the_workspace_and_opens_it_synced() {
    let t = team_fixture_with("write", false);
    let pilot = Pilot::start().await;
    pilot.ok("launch", t.launch_args()).await;
    until(&pilot, "the explorer", |tr| {
        find_node(tr, "connection-list").is_some()
    })
    .await;
    pilot.ok("key", json!({ "keys": "ctrl-shift-p" })).await;
    until(&pilot, "the palette", |tr| {
        find_node(tr, "palette").is_some()
    })
    .await;
    pilot.ok("type", json!({ "text": "join team" })).await;
    pilot.ok("key", json!({ "keys": "enter" })).await;
    until(&pilot, "the dialog", |tr| {
        find_node(tr, "team-server").is_some()
    })
    .await;
    type_into(&pilot, "team-server", t.stub.uri()).await;
    type_into(&pilot, "team-user", "ada").await;
    type_into(&pilot, "team-password", "pw").await;
    pilot
        .ok("click", json!({ "target": { "id": ["team-sign-in"] } }))
        .await;
    let row = part_a11y_id("team-workspace", "acme/shared");
    until(&pilot, "the list", |tr| find_node(tr, &row).is_some()).await;
    pilot
        .ok("click", json!({ "target": { "id": [row] } }))
        .await;
    let target = t.root.join("joined");
    type_into(&pilot, "team-folder", target.to_str().unwrap()).await;
    pilot
        .ok("click", json!({ "target": { "id": ["team-join"] } }))
        .await;
    until(&pilot, "synced in the joined folder", |tr| {
        name(tr, "team-status") == Some("Synced")
    })
    .await;
    assert!(target.join(".git").is_dir());
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    pilot.assert_bus_untouched();
}
