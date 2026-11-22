//! The conflict banner under gpui-pilot: both sides changed a query; Compare opens the diff,
//! Keep mine pushes mine.
mod common;

use std::time::{Duration, Instant};

use common::team::{name, team_fixture, until};
use common::{Pilot, find_node};
use dbento_app::explorer::part_a11y_id;
use serde_json::json;

#[tokio::test(flavor = "multi_thread")]
async fn compare_then_keep_mine_pushes_my_side() {
    let t = team_fixture("write");
    t.local_commit("queries/a.sql", "select 2;\n");
    t.teammate("queries/a.sql", "select 3;\n");
    let pilot = Pilot::start().await;
    pilot.ok("launch", t.launch_args()).await;
    let compare = part_a11y_id("team-conflict-compare", "queries/a.sql");
    until(&pilot, "the banner", |tr| find_node(tr, &compare).is_some()).await;
    assert_eq!(
        name(&pilot.ok("tree", json!({})).await, "team-status"),
        Some("Conflict in 1 file")
    );
    pilot
        .ok("click", json!({ "target": { "id": [compare] } }))
        .await;
    until(&pilot, "the compare dialog", |tr| {
        find_node(tr, "confirm-dialog").is_some()
    })
    .await;
    pilot
        .ok(
            "click",
            json!({ "target": { "id": ["team-compare-cancel"] } }),
        )
        .await;
    until(&pilot, "the dialog to close", |tr| {
        find_node(tr, "confirm-dialog").is_none()
    })
    .await;

    let keep = part_a11y_id("team-conflict-keep", "queries/a.sql");
    pilot
        .ok("click", json!({ "target": { "id": [keep] } }))
        .await;
    until(&pilot, "synced", |tr| {
        name(tr, "team-status") == Some("Synced")
    })
    .await;
    let deadline = Instant::now() + Duration::from_secs(10);
    while t.origin_show("queries/a.sql").as_deref() != Some("select 2;\n") {
        assert!(
            Instant::now() < deadline,
            "my side never reached the origin"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    pilot.assert_bus_untouched();
}
