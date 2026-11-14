//! The session backend's accessibility tree: the approval dialog's buttons, the window
//! controls, the tab strip and the start card each show by id and name, and a click by id
//! reaches them.
mod common;

use std::time::{Duration, Instant};

use common::{Pilot, find_node, launch_args, session_app, sqlite_workspace};
use dbento_core::{ApprovalRequest, ApprovalStore, Writes};
use serde_json::{Value, json};

async fn until(pilot: &Pilot, what: &str, done: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let tree = pilot.ok("tree", json!({})).await;
        if done(&tree) {
            return tree;
        }
        assert!(Instant::now() < deadline, "{what} never happened: {tree}");
        pilot.ok("wait", json!({ "ms": 100 })).await;
    }
}

fn name<'a>(tree: &'a Value, id: &str) -> Option<&'a str> {
    find_node(tree, id).and_then(|n| n["name"].as_str())
}

fn assert_names(tree: &Value, want: &[(&str, &str)]) {
    for (id, label) in want {
        assert_eq!(name(tree, id), Some(*label), "{id} in {tree}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_session_tree_names_the_chrome_and_the_approval_buttons() {
    let app = session_app();
    assert!(app.is_file(), "no optimised app at {}", app.display());
    let ws = sqlite_workspace();
    // The state lives inside the workspace, which the session can read and write.
    let state = ws.path().join(".state");
    let store = ApprovalStore::open(&dbento_app::app::AppPaths::in_dir(&state).approvals).unwrap();
    let request = ApprovalRequest::new(
        ws.path(),
        "lite",
        "INSERT INTO t VALUES (1)",
        Writes::Yes,
        vec![],
    );
    store.request(&request).unwrap();
    let pilot = Pilot::start().await;
    let mut args = launch_args("session", ws.path());
    args["env"] = json!({ "DBENTO_STATE_DIR": state });
    pilot.ok("launch", args).await;

    let tree = until(&pilot, "the approval buttons", |t| {
        name(t, "approval-approve").is_some()
    })
    .await;
    assert_names(
        &tree,
        &[("approval-approve", "Approve"), ("approval-deny", "Deny")],
    );
    // The session draws client-side controls, named above the dialog as beneath it.
    assert_names(
        &tree,
        &[("window-minimize", "Minimize"), ("window-close", "Close")],
    );
    pilot
        .ok("click", json!({ "target": { "id": ["approval-deny"] } }))
        .await;

    let tree = until(&pilot, "the start card", |t| {
        find_node(t, "approval-approve").is_none() && name(t, "start-action-0").is_some()
    })
    .await;
    assert_names(
        &tree,
        &[
            ("start-action-0", "New tab"),
            ("start-action-3", "History"),
            ("window-minimize", "Minimize"),
            ("window-close", "Close"),
            ("connection-new", "New connection"),
            ("schema-refresh", "Refresh"),
            ("title-search", "Search tables, queries and commands"),
        ],
    );
    // New tab opens on the selected connection.
    for id in ["connection-lite", "start-action-0"] {
        pilot.ok("click", json!({ "target": { "id": [id] } })).await;
    }
    let tree = until(&pilot, "a tab", |t| name(t, "tab-0").is_some()).await;
    assert_names(
        &tree,
        &[("tab-close-0", "Close tab"), ("tab-new", "New tab")],
    );
    pilot.ok("close", json!({})).await;
    pilot.assert_bus_untouched();
}
