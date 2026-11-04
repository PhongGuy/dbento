//! Palette Escape under gpui-pilot: a palette opened by its shortcut closes on one Escape.
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

#[tokio::test(flavor = "multi_thread")]
async fn one_escape_closes_the_palette_a_shortcut_opened() {
    let ws = sqlite_workspace();
    let pilot = Pilot::start().await;
    pilot.ok("launch", launch_args("headless", ws.path())).await;
    pilot
        .ok(
            "click",
            json!({ "target": { "id": ["connection-lite"] }, "count": 2 }),
        )
        .await;
    until(&pilot, "the editor", |t| find_node(t, "editor").is_some()).await;
    pilot
        .ok("click", json!({ "target": { "id": ["editor"] } }))
        .await;

    // Each shortcut seeds its mode prefix ("# ", "> "); one Escape must still close.
    for keys in ["ctrl-o", "ctrl-shift-p"] {
        pilot.ok("key", json!({ "keys": keys })).await;
        until(&pilot, "the palette", |t| find_node(t, "palette").is_some()).await;
        pilot.ok("key", json!({ "keys": "escape" })).await;
        until(&pilot, "the close", |t| find_node(t, "palette").is_none()).await;
    }
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    pilot.assert_bus_untouched();
}
