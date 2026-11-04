//! Save over a disk change under gpui-pilot: Ctrl+S on a query whose file changed on disk asks
//! first, and Overwrite writes; the next save does not ask again.
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

async fn key(pilot: &Pilot, keys: &str) {
    pilot.ok("key", json!({ "keys": keys })).await;
}

// Types `text` at the editor's caret, the end of its one line, and presses Ctrl+S.
async fn save(pilot: &Pilot, text: &str) {
    pilot
        .ok("click", json!({ "target": { "id": ["editor"] } }))
        .await;
    pilot
        .ok(
            "type",
            json!({ "target": { "id": ["editor"] }, "text": text }),
        )
        .await;
    key(pilot, "ctrl-s").await;
}

fn read(path: &std::path::Path) -> String {
    std::fs::read_to_string(path).unwrap()
}

async fn file_holds(pilot: &Pilot, path: &std::path::Path, text: &str) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while read(path) != text {
        assert!(
            Instant::now() < deadline,
            "the file never held {text:?}: {:?}",
            read(path)
        );
        pilot.ok("wait", json!({ "ms": 50 })).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn saving_over_a_file_changed_on_disk_asks_then_overwrites() {
    let ws = sqlite_workspace();
    let file = ws.path().join("queries/lite/a.sql");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "SELECT 1").unwrap();
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
    key(&pilot, "ctrl-o").await;
    until(&pilot, "the palette", |t| find_node(t, "palette").is_some()).await;
    pilot.ok("wait", json!({ "ms": 300 })).await;
    key(&pilot, "enter").await;
    until(&pilot, "the opened file", |t| {
        find_node(t, "palette").is_none()
    })
    .await;

    std::fs::write(&file, "SELECT 9").unwrap();
    save(&pilot, " -- a").await;
    until(&pilot, "the conflict question", |t| {
        find_node(t, "save-conflict-cancel").is_some()
            && find_node(t, "save-conflict-reload").is_some()
    })
    .await;
    assert_eq!(
        read(&file),
        "SELECT 9",
        "nothing is written before the answer"
    );
    pilot
        .ok(
            "click",
            json!({ "target": { "id": ["save-conflict-overwrite"] } }),
        )
        .await;
    file_holds(&pilot, &file, "SELECT 1 -- a").await;

    save(&pilot, " -- b").await;
    file_holds(&pilot, &file, "SELECT 1 -- a -- b").await;
    let tree = pilot.ok("tree", json!({})).await;
    assert!(find_node(&tree, "confirm-dialog").is_none(), "{tree}");
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
}
