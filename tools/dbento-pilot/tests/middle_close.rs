//! A middle click on a tab closes it as its close button does: an unedited tab at once, an
//! edited one only after the discard question.
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

async fn click(pilot: &Pilot, id: &str, extra: Value) {
    let mut args = json!({ "target": { "id": [id] } });
    if let (Some(a), Some(e)) = (args.as_object_mut(), extra.as_object()) {
        a.extend(e.clone());
    }
    pilot.ok("click", args).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_middle_click_closes_a_clean_tab_and_asks_for_an_edited_one() {
    let ws = sqlite_workspace();
    let pilot = Pilot::start().await;
    pilot.ok("launch", launch_args("headless", ws.path())).await;
    click(&pilot, "connection-lite", json!({ "count": 2 })).await;
    until(&pilot, "the first tab", |t| find_node(t, "tab-0").is_some()).await;
    click(&pilot, "tab-new", json!({})).await;
    until(&pilot, "the second tab", |t| {
        find_node(t, "tab-1").is_some()
    })
    .await;
    pilot
        .ok(
            "type",
            json!({ "target": { "id": ["editor"] }, "text": "SELECT 42" }),
        )
        .await;

    // The first tab is clean, so it goes without a question and the edited one moves to 0.
    click(&pilot, "tab-0", json!({ "button": "middle" })).await;
    until(&pilot, "the clean close", |t| {
        find_node(t, "tab-1").is_none() && find_node(t, "tab-0").is_some()
    })
    .await;
    let tree = pilot.ok("tree", json!({})).await;
    assert!(find_node(&tree, "confirm-dialog").is_none(), "{tree}");

    click(&pilot, "tab-0", json!({ "button": "middle" })).await;
    until(&pilot, "the discard question", |t| {
        find_node(t, "confirm-dialog").is_some()
    })
    .await;
    click(&pilot, "discard-keep", json!({})).await;
    until(&pilot, "the kept tab", |t| {
        find_node(t, "confirm-dialog").is_none() && find_node(t, "tab-0").is_some()
    })
    .await;
}
