//! Sign out and back in under gpui-pilot: the palette signs out, the chip says so, and its
//! click signs the same account in again.
mod common;

use common::team::{name, team_fixture, until};
use common::{Pilot, find_node};
use serde_json::json;

#[tokio::test(flavor = "multi_thread")]
async fn sign_out_from_the_palette_then_back_in_from_the_chip() {
    let t = team_fixture("write");
    let pilot = Pilot::start().await;
    pilot.ok("launch", t.launch_args()).await;
    until(&pilot, "synced", |tr| {
        name(tr, "team-status") == Some("Synced")
    })
    .await;
    pilot.ok("key", json!({ "keys": "ctrl-shift-p" })).await;
    until(&pilot, "the palette", |tr| {
        find_node(tr, "palette").is_some()
    })
    .await;
    pilot
        .ok("type", json!({ "text": "sign out of team" }))
        .await;
    pilot.ok("key", json!({ "keys": "enter" })).await;
    until(&pilot, "signed out", |tr| {
        name(tr, "team-status") == Some("Signed out")
    })
    .await;
    assert_eq!(t.stub.hits("DELETE", "/api/v1/tokens/42"), 1);

    pilot
        .ok("click", json!({ "target": { "id": ["team-status"] } }))
        .await;
    until(&pilot, "the sign-in dialog", |tr| {
        find_node(tr, "team-password").is_some()
    })
    .await;
    pilot
        .ok(
            "type",
            json!({ "target": { "id": ["team-password"] }, "text": "pw" }),
        )
        .await;
    pilot
        .ok("click", json!({ "target": { "id": ["team-sign-in"] } }))
        .await;
    until(&pilot, "synced again", |tr| {
        name(tr, "team-status") == Some("Synced")
    })
    .await;
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    pilot.assert_bus_untouched();
}
