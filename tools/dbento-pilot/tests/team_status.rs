//! The team chip under gpui-pilot: a bound workspace syncs, and a teammate's push announced
//! by a ref notice reaches the explorer through the reload path.
mod common;

use common::team::{name, team_fixture, until};
use common::{Pilot, find_node};
use serde_json::json;

#[tokio::test(flavor = "multi_thread")]
async fn the_chip_shows_synced_and_a_teammates_connection_arrives() {
    let t = team_fixture("write");
    let pilot = Pilot::start().await;
    pilot.ok("launch", t.launch_args()).await;
    until(&pilot, "synced", |tr| {
        name(tr, "team-status") == Some("Synced")
    })
    .await;
    assert!(find_node(&pilot.ok("tree", json!({})).await, "team-status-retry").is_none());

    let more = format!(
        "{}\n[connections.lite2]\nengine = \"sqlite\"\npath = \"app.db\"\n",
        common::team::TOML
    );
    t.teammate("dbento.toml", &more);
    t.notify_ref();
    until(&pilot, "the teammate's connection", |tr| {
        find_node(tr, "connection-lite2").is_some()
    })
    .await;
    until(&pilot, "synced after the pull", |tr| {
        name(tr, "team-status") == Some("Synced")
    })
    .await;
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    pilot.assert_bus_untouched();
}
