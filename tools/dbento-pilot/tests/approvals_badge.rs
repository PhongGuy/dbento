//! The rail approvals badge under gpui-pilot: it follows the pending count while the
//! approval dialog waits, and caps at a 99+ pill. Set DBENTO_SHOTS_DIR to keep the shots.
mod common;

use std::path::Path;
use std::time::{Duration, Instant, SystemTime};

use common::{Pilot, bounds, find_node, launch_args, sqlite_workspace};
use dbento_app::app::AppPaths;
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
        pilot.ok("wait", json!({ "ms": 50 })).await;
    }
}

async fn shot(pilot: &Pilot, name: &str) {
    if let Some(dir) = std::env::var_os("DBENTO_SHOTS_DIR").map(std::path::PathBuf::from) {
        std::fs::create_dir_all(&dir).unwrap();
        pilot
            .ok("screenshot", json!({ "save_to": dir.join(name) }))
            .await;
    }
}

fn badge(tree: &Value) -> Option<&str> {
    find_node(tree, "rail-approvals-badge").and_then(|n| n["name"].as_str())
}

// Files requests numbered `from..to` against the `lite` connection of `ws`, oldest first.
fn file(state: &Path, ws: &Path, from: usize, to: usize) {
    let store = ApprovalStore::open(&AppPaths::in_dir(state).approvals).unwrap();
    for i in from..to {
        let sql = format!("INSERT INTO t VALUES ({i})");
        store
            .request(
                &ApprovalRequest::new(ws, "lite", &sql, Writes::Yes, vec![])
                    .with_time(SystemTime::now() - Duration::from_secs((to - i) as u64)),
            )
            .unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_badge_follows_the_queue_behind_the_dialog_and_caps_at_99_plus() {
    let ws = sqlite_workspace();
    let state = tempfile::tempdir().unwrap();
    file(state.path(), ws.path(), 0, 1);
    let pilot = Pilot::start().await;
    let mut args = launch_args("headless", ws.path());
    args["env"] = json!({ "DBENTO_STATE_DIR": state.path() });
    pilot.ok("launch", args).await;
    let tree = until(&pilot, "the dialog", |t| {
        find_node(t, "approval-dialog").is_some() && badge(t) == Some("1")
    })
    .await;
    let round = bounds(&tree, "rail-approvals-badge");
    shot(&pilot, "badge-1.png").await;

    // More requests land while the dialog still waits on the first.
    file(state.path(), ws.path(), 1, 120);
    pilot.ok("wait", json!({ "ms": 2500 })).await;
    shot(&pilot, "badge-120.png").await;
    let tree = until(&pilot, "the 99+ badge", |t| badge(t) == Some("99+")).await;
    assert!(find_node(&tree, "approval-dialog").is_some());
    let pill = bounds(&tree, "rail-approvals-badge");
    let button = bounds(&tree, "rail-approvals");
    assert!(
        pill.w > round.w,
        "the pill {pill:?} is no wider than {round:?}"
    );
    assert_eq!(pill.h, round.h);
    assert!(
        pill.x >= button.x,
        "the pill {pill:?} sticks out of the button {button:?}"
    );
}

fn label<'a>(tree: &'a Value, id: &str) -> Option<&'a str> {
    find_node(tree, id).and_then(|n| n["name"].as_str())
}

#[tokio::test(flavor = "multi_thread")]
async fn answering_one_request_opens_the_next_with_a_live_count() {
    let ws = sqlite_workspace();
    let state = tempfile::tempdir().unwrap();
    file(state.path(), ws.path(), 0, 1);
    let pilot = Pilot::start().await;
    let mut args = launch_args("headless", ws.path());
    args["env"] = json!({ "DBENTO_STATE_DIR": state.path() });
    pilot.ok("launch", args).await;
    let tree = until(&pilot, "the first request", |t| {
        label(t, "approval-sql") == Some("INSERT INTO t VALUES (0)") && badge(t) == Some("1")
    })
    .await;
    assert!(
        find_node(&tree, "approval-queue").is_none(),
        "a lone request has no count"
    );

    // Two more land while it waits: the count follows the badge.
    file(state.path(), ws.path(), 1, 3);
    until(&pilot, "the count of three", |t| {
        badge(t) == Some("3") && label(t, "approval-queue") == Some("1 of 3")
    })
    .await;

    // Nothing but the answer: the next request opens on its own.
    pilot
        .ok("click", json!({ "target": { "id": ["approval-approve"] } }))
        .await;
    until(&pilot, "the second request", |t| {
        label(t, "approval-sql") == Some("INSERT INTO t VALUES (1)")
            && label(t, "approval-queue") == Some("1 of 2")
            && badge(t) == Some("2")
    })
    .await;
    pilot
        .ok("click", json!({ "target": { "id": ["approval-deny"] } }))
        .await;
    let tree = until(&pilot, "the last request", |t| {
        label(t, "approval-sql") == Some("INSERT INTO t VALUES (2)") && badge(t) == Some("1")
    })
    .await;
    assert!(
        find_node(&tree, "approval-queue").is_none(),
        "a lone request has no count"
    );
}
