//! Closing the window while a statement runs asks first, naming the tab; Keep leaves the run
//! going and Close quits. Set DBENTO_SHOTS_DIR to keep the shot of the question.
mod common;

use std::time::{Duration, Instant};

use common::{Pilot, find_node, launch_args, sqlite_workspace};
use serde_json::{Value, json};

// Counts forever, so the statement is still running whenever the test looks.
const LONG: &str =
    "WITH RECURSIVE r(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM r) SELECT count(*) FROM r";

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

async fn shot(pilot: &Pilot, name: &str) {
    if let Some(dir) = std::env::var_os("DBENTO_SHOTS_DIR").map(std::path::PathBuf::from) {
        std::fs::create_dir_all(&dir).unwrap();
        pilot
            .ok("screenshot", json!({ "save_to": dir.join(name) }))
            .await;
    }
}

// A window close the app refuses answers `closed: false` and leaves the window up.
async fn close_window(pilot: &Pilot) {
    let reply = pilot
        .ok("close", json!({ "mode": "window", "timeout_ms": 500 }))
        .await;
    assert_eq!(reply["closed"], json!(false), "{reply}");
}

#[tokio::test(flavor = "multi_thread")]
async fn closing_while_a_statement_runs_asks_naming_the_tab() {
    let ws = sqlite_workspace();
    let pilot = Pilot::start().await;
    pilot.ok("launch", launch_args("headless", ws.path())).await;
    click(&pilot, "connection-lite", json!({ "count": 2 })).await;
    pilot
        .ok("tree", json!({ "wait_for": { "id": ["editor"] } }))
        .await;
    pilot
        .ok(
            "type",
            json!({ "target": { "id": ["editor"] }, "text": LONG }),
        )
        .await;
    click(&pilot, "run-all", json!({})).await;
    until(&pilot, "the run's Stop", |t| {
        find_node(t, "cancel").is_some()
    })
    .await;

    close_window(&pilot).await;
    let tree = until(&pilot, "the question", |t| {
        find_node(t, "confirm-dialog").is_some()
    })
    .await;
    let detail = find_node(&tree, "confirm-detail").map(|n| n["name"].clone());
    assert_eq!(
        detail,
        Some(json!(
            "Closing Dbento cancels the statement running in Query 1."
        )),
        "{tree}"
    );
    pilot.ok("wait", json!({ "ms": 300 })).await;
    shot(&pilot, "quit-running.png").await;

    click(&pilot, "discard-keep", json!({})).await;
    until(&pilot, "the kept run", |t| {
        find_node(t, "confirm-dialog").is_none() && find_node(t, "cancel").is_some()
    })
    .await;

    close_window(&pilot).await;
    until(&pilot, "the question again", |t| {
        find_node(t, "discard-close").is_some()
    })
    .await;
    click(&pilot, "discard-close", json!({})).await;
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
}
