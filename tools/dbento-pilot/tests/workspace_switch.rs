//! Workspace switching under gpui-pilot: Open workspace from the palette swaps the window to
//! the picked folder, and Close workspace from the title pill's menu returns to the welcome view.
mod common;

use std::time::{Duration, Instant};

use common::{Pilot, find_node, launch_args, sqlite_workspace_named};
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
fn name<'a>(tree: &'a Value, id: &str) -> Option<&'a str> {
    find_node(tree, id).and_then(|n| n["name"].as_str())
}
async fn click(pilot: &Pilot, id: &str) {
    pilot.ok("click", json!({ "target": { "id": [id] } })).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn open_from_the_palette_then_close_from_the_title_pill() {
    let (_a, first) = sqlite_workspace_named("first-ws");
    let (_b, second) = sqlite_workspace_named("second-ws");
    // The app's own state dir, so Close workspace never touches the user's next start.
    let state = tempfile::tempdir().unwrap();
    let pilot = Pilot::start().await;
    let mut args = launch_args("headless", &first);
    args["env"] = json!({ "DBENTO_STATE_DIR": state.path() });
    pilot.ok("launch", args).await;
    until(&pilot, "the first workspace", |t| {
        name(t, "title-workspace") == Some("first-ws")
    })
    .await;

    pilot.ok("key", json!({ "keys": "ctrl-shift-p" })).await;
    until(&pilot, "the palette", |t| find_node(t, "palette").is_some()).await;
    pilot.ok("type", json!({ "text": "open workspace" })).await;
    pilot.ok("key", json!({ "keys": "enter" })).await;
    pilot
        .ok("pick_file", json!({ "paths": [&second], "settle_ms": 0 }))
        .await;
    until(&pilot, "the second workspace", |t| {
        name(t, "title-workspace") == Some("second-ws")
    })
    .await;

    click(&pilot, "title-workspace").await;
    until(&pilot, "the workspace menu", |t| {
        find_node(t, "title-workspace-close").is_some()
            && find_node(t, "title-workspace-recent-0").is_some()
    })
    .await;
    click(&pilot, "title-workspace-close").await;
    until(&pilot, "the welcome view", |t| {
        find_node(t, "welcome-open").is_some() && find_node(t, "title-workspace").is_none()
    })
    .await;
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    pilot.assert_bus_untouched();
}
