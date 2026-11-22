//! The refusal panel under gpui-pilot: a push the origin's hook refuses lists its finding.
mod common;

use common::team::{name, team_fixture, until};
use common::{Pilot, find_node};
use dbento_app::explorer::part_a11y_id;
use serde_json::json;

#[tokio::test(flavor = "multi_thread")]
async fn a_refused_push_lists_its_finding() {
    let t = team_fixture("write");
    t.hook("pre-receive", "cat >/dev/null\necho 'potluck-reject secret_value refs/heads/main dbento.toml password' >&2\nexit 1");
    t.local_commit("queries/a.sql", "-- password hunter2\nselect 1;\n");
    let pilot = Pilot::start().await;
    pilot.ok("launch", t.launch_args()).await;
    let row = part_a11y_id("team-refusal-row", "0");
    let tree = until(&pilot, "the panel", |tr| find_node(tr, &row).is_some()).await;
    assert_eq!(
        name(&tree, &row),
        Some("dbento.toml on refs/heads/main: a password")
    );
    assert_eq!(name(&tree, "team-status"), Some("Push refused"));
    assert!(find_node(&tree, "team-refusal-retry").is_some());
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    pilot.assert_bus_untouched();
}
