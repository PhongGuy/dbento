//! The tab keys in the running app: Ctrl+Tab in recent order, Ctrl+digits, the move keys, and
//! a drag that reorders the strip.
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

async fn key(pilot: &Pilot, keys: &str) {
    pilot.ok("key", json!({ "keys": keys })).await;
}

/// The strip's tab names, left to right, and the index of the selected one.
fn strip(tree: &Value) -> (Vec<String>, Option<usize>) {
    let mut names = Vec::new();
    let mut selected = None;
    for ix in 0.. {
        let Some(tab) = find_node(tree, &format!("tab-{ix}")) else {
            break;
        };
        names.push(tab["name"].as_str().unwrap_or_default().to_owned());
        if tab["selected"] == json!(true) {
            selected = Some(ix);
        }
    }
    (names, selected)
}

async fn selected(pilot: &Pilot, want: usize) {
    until(pilot, &format!("tab {want} active"), |t| {
        strip(t).1 == Some(want)
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn tab_keys_switch_jump_and_move_and_a_drag_reorders() {
    let ws = sqlite_workspace();
    let pilot = Pilot::start().await;
    pilot.ok("launch", launch_args("headless", ws.path())).await;
    click(&pilot, "connection-lite", json!({ "count": 2 })).await;
    until(&pilot, "the first tab", |t| find_node(t, "tab-0").is_some()).await;
    click(&pilot, "tab-new", json!({})).await;
    click(&pilot, "tab-new", json!({})).await;
    let tree = until(&pilot, "three tabs", |t| strip(t).0.len() == 3).await;
    let (names, active) = strip(&tree);
    assert_eq!(active, Some(2), "{names:?}");

    // Recent order 2, 1, 0: two presses walk to the oldest, and any other key commits.
    key(&pilot, "ctrl-tab").await;
    selected(&pilot, 1).await;
    key(&pilot, "ctrl-tab").await;
    selected(&pilot, 0).await;
    key(&pilot, "escape").await;
    key(&pilot, "ctrl-tab").await;
    selected(&pilot, 2).await;
    key(&pilot, "escape").await;
    // Recent order 2, 0, 1: a reversed walk starts at the oldest.
    key(&pilot, "ctrl-shift-tab").await;
    selected(&pilot, 1).await;
    key(&pilot, "escape").await;

    key(&pilot, "ctrl-1").await;
    selected(&pilot, 0).await;
    key(&pilot, "ctrl-9").await;
    selected(&pilot, 2).await;

    key(&pilot, "ctrl-shift-pageup").await;
    let tree = until(&pilot, "the move left", |t| strip(t).1 == Some(1)).await;
    let moved = vec![names[0].clone(), names[2].clone(), names[1].clone()];
    assert_eq!(strip(&tree).0, moved);

    pilot
        .ok(
            "drag",
            json!({ "from": { "id": ["tab-0"] }, "to": { "id": ["tab-2"] } }),
        )
        .await;
    let dragged = vec![names[2].clone(), names[1].clone(), names[0].clone()];
    let tree = until(&pilot, "the drag reorder", |t| strip(t).0 == dragged).await;
    assert_eq!(strip(&tree).1, Some(0), "the active tab stays active");
}
