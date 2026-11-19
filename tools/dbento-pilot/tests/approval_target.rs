//! The approval dialog and panel name each request's target, and a target changed since the
//! AI asked reads as production with a note. Set DBENTO_SHOTS_DIR to keep the shots.
mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use common::{Pilot, approval_target, find_node, launch_args, sqlite_workspace};
use dbento_app::app::AppPaths;
use dbento_core::{ApprovalRequest, ApprovalStore, Writes};
use serde_json::{Value, json};

fn name<'a>(tree: &'a Value, id: &str) -> Option<&'a str> {
    find_node(tree, id).and_then(|n| n["name"].as_str())
}

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

async fn shot(pilot: &Pilot, file: &str) {
    if let Some(dir) = std::env::var_os("DBENTO_SHOTS_DIR").map(std::path::PathBuf::from) {
        std::fs::create_dir_all(&dir).unwrap();
        pilot
            .ok("screenshot", json!({ "save_to": dir.join(file) }))
            .await;
    }
}

fn file(state: &Path, ws: &Path, sql: &str) {
    let store = ApprovalStore::open(&AppPaths::in_dir(state).approvals).unwrap();
    let target = approval_target(ws, "lite");
    store
        .request(&ApprovalRequest::new(
            ws,
            "lite",
            target,
            sql,
            Writes::Yes,
            vec![],
        ))
        .unwrap();
}

async fn launch(pilot: &Pilot, ws: &Path, state: &Path) {
    let mut args = launch_args("headless", ws);
    args["env"] = json!({ "DBENTO_STATE_DIR": state });
    pilot.ok("launch", args).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_dialog_and_panel_name_the_target() {
    let ws = sqlite_workspace();
    let state = tempfile::tempdir().unwrap();
    file(state.path(), ws.path(), "UPDATE t SET a = 1 WHERE a = 2");
    let pilot = Pilot::start().await;
    launch(&pilot, ws.path(), state.path()).await;
    let tree = until(&pilot, "the dialog", |t| {
        name(t, "approval-target").is_some()
    })
    .await;
    assert_eq!(name(&tree, "approval-target"), Some("app.db"));
    assert!(find_node(&tree, "approval-changed").is_none());
    assert!(find_node(&tree, "approval-production").is_none());
    shot(&pilot, "approval-target-normal.png").await;
    pilot
        .ok("click", json!({ "target": { "id": ["approval-approve"] } }))
        .await;
    until(&pilot, "the dialog to close", |t| {
        find_node(t, "approval-dialog").is_none()
    })
    .await;
    pilot
        .ok("click", json!({ "target": { "id": ["rail-approvals"] } }))
        .await;
    until(&pilot, "the decided row", |t| {
        name(t, "approvals-decided-0")
            .is_some_and(|n| n.starts_with("lite \u{B7} app.db \u{B7} UPDATE"))
    })
    .await;
    // A second request while the panel is open: its row draws under the new dialog.
    file(state.path(), ws.path(), "DELETE FROM t WHERE a = 3");
    until(&pilot, "the pending row", |t| {
        name(t, "approvals-pending-0")
            .is_some_and(|n| n.starts_with("lite \u{B7} app.db \u{B7} DELETE"))
            && find_node(t, "approval-dialog").is_some()
    })
    .await;
    shot(&pilot, "approval-target-panel.png").await;
    pilot
        .ok("click", json!({ "target": { "id": ["approval-deny"] } }))
        .await;
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    pilot.assert_bus_untouched();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_target_changed_before_launch_reads_as_production_with_a_note() {
    let ws = sqlite_workspace();
    let state = tempfile::tempdir().unwrap();
    // Filed against app.db; the name then moves to another file before the app first looks.
    file(state.path(), ws.path(), "DELETE FROM t WHERE a = 3");
    std::fs::write(ws.path().join("other.db"), b"").unwrap();
    let toml = ws.path().join("dbento.toml");
    let text = std::fs::read_to_string(&toml)
        .unwrap()
        .replace("app.db", "other.db");
    std::fs::write(&toml, text).unwrap();
    let pilot = Pilot::start().await;
    launch(&pilot, ws.path(), state.path()).await;
    let tree = until(&pilot, "the changed dialog", |t| {
        find_node(t, "approval-changed").is_some()
    })
    .await;
    assert!(find_node(&tree, "approval-production").is_some());
    assert_eq!(name(&tree, "approval-target"), Some("app.db"));
    shot(&pilot, "approval-target-changed.png").await;
    pilot
        .ok("click", json!({ "target": { "id": ["approval-deny"] } }))
        .await;
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    pilot.assert_bus_untouched();
}
