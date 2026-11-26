//! QA under gpui-pilot: editing a connection's port says its saved password stays behind.
mod common;

use std::time::{Duration, Instant};

use common::{Pilot, find_node, launch_args};
use serde_json::{Value, json};

const NOTE: &str =
    "The saved password is not sent to the changed server; type it again to save it for this one.";

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
async fn a_changed_port_shows_the_pin_note() {
    let ws = tempfile::tempdir().unwrap();
    // No port, so the dialog's Port field starts empty and typing into it sets the whole value.
    let toml = "dbento = 1\n\n[connections.pg]\nengine = \"postgres\"\nhost = \"127.0.0.1\"\ndatabase = \"x\"\nuser = \"x\"\n";
    std::fs::write(ws.path().join("dbento.toml"), toml).unwrap();
    let pilot = Pilot::start().await;
    pilot.ok("launch", launch_args("headless", ws.path())).await;
    until(&pilot, "the row", |t| {
        find_node(t, "connection-pg").is_some()
    })
    .await;
    // The edit button shows on the hovered row.
    pilot
        .ok("hover", json!({ "target": { "id": ["connection-pg"] } }))
        .await;
    pilot
        .ok(
            "tree",
            json!({ "wait_for": { "id": ["edit-connection-pg"] } }),
        )
        .await;
    pilot
        .ok(
            "click",
            json!({ "target": { "id": ["edit-connection-pg"] } }),
        )
        .await;
    let tree = until(&pilot, "the dialog", |t| {
        find_node(t, "conn-name").is_some()
    })
    .await;
    assert!(
        find_node(&tree, "conn-pin-note").is_none(),
        "no note before an edit: {tree}"
    );
    pilot
        .ok(
            "type",
            json!({ "target": { "id": ["conn-port"] }, "text": "6543" }),
        )
        .await;
    let tree = until(&pilot, "the note", |t| {
        find_node(t, "conn-pin-note").is_some()
    })
    .await;
    let shown = find_node(&tree, "conn-pin-note").and_then(|n| n["name"].as_str());
    assert_eq!(shown, Some(NOTE), "the note names what happens: {tree}");
    if let Some(dir) = std::env::var_os("DBENTO_SHOTS_DIR").map(std::path::PathBuf::from) {
        std::fs::create_dir_all(&dir).unwrap();
        pilot
            .ok("screenshot", json!({ "save_to": dir.join("pin-note.png") }))
            .await;
    }
    pilot
        .ok("click", json!({ "target": { "id": ["conn-cancel"] } }))
        .await;
    assert_eq!(
        pilot.ok("close", json!({})).await["exit_code"],
        json!(0),
        "clean exit"
    );
    pilot.assert_bus_untouched();
}
