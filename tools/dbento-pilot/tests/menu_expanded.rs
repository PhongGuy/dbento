//! Menu buttons under gpui-pilot: the title pill, the query tab's connection chip and the
//! history filter report expanded while their menu shows and collapsed once it closes.
mod common;

use std::time::{Duration, Instant};

use common::{Pilot, find_node, launch_args, sqlite_workspace};
use serde_json::{Value, json};

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
fn expanded(tree: &Value, id: &str) -> Option<bool> {
    find_node(tree, id).and_then(|n| n["expanded"].as_bool())
}
async fn click(pilot: &Pilot, id: &str) {
    pilot.ok("click", json!({ "target": { "id": [id] } })).await;
}

// Opens `button`'s menu, waits for its first row `row`, and checks the button says expanded.
async fn open(pilot: &Pilot, button: &str, row: &str) {
    let tree = until(pilot, button, |t| find_node(t, button).is_some()).await;
    assert_eq!(
        expanded(&tree, button),
        Some(false),
        "{button} starts collapsed"
    );
    click(pilot, button).await;
    until(pilot, &format!("{button} expanded"), |t| {
        find_node(t, row).is_some() && expanded(t, button) == Some(true)
    })
    .await;
}
async fn closed(pilot: &Pilot, button: &str, row: &str) {
    until(pilot, &format!("{button} collapsed"), |t| {
        find_node(t, row).is_none() && expanded(t, button) == Some(false)
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn menu_buttons_report_expanded_then_collapsed() {
    let ws = sqlite_workspace();
    // The app's own state dir, so the opened history panel never reaches the user's next start.
    let state = tempfile::tempdir().unwrap();
    let pilot = Pilot::start().await;
    let mut args = launch_args("headless", ws.path());
    args["env"] = json!({ "DBENTO_STATE_DIR": state.path() });
    pilot.ok("launch", args).await;

    open(&pilot, "title-workspace", "title-workspace-close").await;
    pilot.ok("key", json!({ "keys": "escape" })).await;
    closed(&pilot, "title-workspace", "title-workspace-close").await;

    common::open_connection(&pilot, "connection-lite").await;
    open(&pilot, "query-connection", "query-connection-pick-0").await;
    pilot.ok("key", json!({ "keys": "escape" })).await;
    closed(&pilot, "query-connection", "query-connection-pick-0").await;

    pilot.ok("key", json!({ "keys": "ctrl-shift-h" })).await;
    open(&pilot, "history-connection", "history-filter-0").await;
    pilot.ok("key", json!({ "keys": "escape" })).await;
    closed(&pilot, "history-connection", "history-filter-0").await;

    // A click outside the open menu closes it too.
    open(&pilot, "history-connection", "history-filter-0").await;
    click(&pilot, "editor").await;
    closed(&pilot, "history-connection", "history-filter-0").await;

    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    pilot.assert_bus_untouched();
}
