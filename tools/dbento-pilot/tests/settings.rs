//! Settings in the running app: Ctrl+, opens it, Compact and a larger grid font change the
//! row heights live, and a relaunch on the same state folder restores both.
//! Set DBENTO_SHOTS_DIR to keep shots of the dialog in dark, Compact, at UI size 16 and in light.
mod common;

use std::time::{Duration, Instant};

use common::{Pilot, bounds, find_node, launch_args, run_query, sqlite_workspace};
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
async fn click(pilot: &Pilot, id: &str) {
    pilot.ok("click", json!({ "target": { "id": [id] } })).await;
}
async fn shot(pilot: &Pilot, name: &str) {
    if let Some(dir) = std::env::var_os("DBENTO_SHOTS_DIR").map(std::path::PathBuf::from) {
        std::fs::create_dir_all(&dir).unwrap();
        pilot
            .ok("screenshot", json!({ "save_to": dir.join(name) }))
            .await;
    }
}
/// Clicks the size field's increment button `times` times; the path skips the kit's own ids.
async fn increment(pilot: &Pilot, field: &str, times: u32) {
    for _ in 0..times {
        pilot
            .ok("click", json!({ "target": { "id": [field, "increment"] } }))
            .await;
    }
}
/// The row element carries the row height; its cells sit 1 px shorter above the row border.
fn height_is(tree: &Value, id: &str, h: f64) -> bool {
    find_node(tree, id).is_some() && (bounds(tree, id).h - h).abs() < 0.5
}

#[tokio::test(flavor = "multi_thread")]
async fn settings_apply_live_and_come_back_after_a_restart() {
    let (ws, state) = (sqlite_workspace(), tempfile::tempdir().unwrap());
    let pilot = Pilot::start().await;
    let mut args = launch_args("headless", ws.path());
    args["env"] = json!({ "DBENTO_STATE_DIR": state.path() });
    pilot.ok("launch", args.clone()).await;
    run_query(&pilot, "SELECT 1 AS n UNION ALL SELECT 2", false).await;
    until(&pilot, "default rows", |t| height_is(t, "row-0", 28.)).await;

    key(&pilot, "ctrl-,").await;
    until(&pilot, "the dialog", |t| {
        find_node(t, "settings-dialog").is_some()
    })
    .await;
    shot(&pilot, "settings-dark.png").await;
    click(&pilot, "settings-density-compact").await;
    until(&pilot, "compact rows", |t| height_is(t, "row-0", 24.)).await;
    shot(&pilot, "settings-compact.png").await;
    // Grid 12 -> 14: ceil(17.5) + 8 = 26 beats the compact base, and differs from every default.
    increment(&pilot, "settings-grid-font-size", 2).await;
    until(&pilot, "the font-driven row", |t| {
        height_is(t, "row-0", 26.)
    })
    .await;
    increment(&pilot, "settings-ui-font-size", 3).await;
    until(&pilot, "the 24 px tree", |t| {
        height_is(t, "connection-lite", 24.)
    })
    .await;
    shot(&pilot, "settings-ui16.png").await;
    click(&pilot, "settings-theme-light").await;
    pilot.ok("wait", json!({ "ms": 200 })).await;
    shot(&pilot, "settings-light.png").await;

    // The save is debounced, so poll until the file holds every change rather than guess a delay.
    let file = state.path().join("settings.json");
    let expected = |s: &Value| {
        s["density"] == json!("compact")
            && s["grid_font"]["size"] == json!(14)
            && s["ui_font"]["size"] == json!(16)
            && s["appearance"] == json!("light")
    };
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let read = std::fs::read_to_string(&file).ok();
        let saved = read
            .as_deref()
            .and_then(|t| serde_json::from_str::<Value>(t).ok());
        if saved.as_ref().is_some_and(expected) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "settings.json never saved: {read:?}"
        );
        pilot.ok("wait", json!({ "ms": 50 })).await;
    }

    pilot.ok("close", json!({ "keep_data": true })).await;
    args["keep_data"] = json!(true);
    pilot.ok("launch", args).await;
    // The tab and its draft come back from the same state folder, so it is run, not retyped.
    pilot
        .ok("tree", json!({ "wait_for": { "id": ["run-all"] } }))
        .await;
    click(&pilot, "run-all").await;
    until(&pilot, "restored rows", |t| {
        height_is(t, "row-0", 26.) && height_is(t, "connection-lite", 24.)
    })
    .await;
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    pilot.assert_bus_untouched();
}
