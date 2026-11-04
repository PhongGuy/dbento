//! uxfix A2b under gpui-pilot: the robot opens the approvals panel with the MCP command.
mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use common::{Pilot, find_node, launch_args, sqlite_workspace};
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

#[tokio::test(flavor = "multi_thread")]
async fn the_robot_opens_the_approvals_panel() {
    let ws = sqlite_workspace();
    let pilot = Pilot::start().await;
    pilot.ok("launch", launch_args("headless", ws.path())).await;
    pilot
        .ok("click", json!({ "target": { "id": ["rail-approvals"] } }))
        .await;
    let tree = until(&pilot, "the panel", |t| {
        find_node(t, "approvals-panel").is_some()
    })
    .await;
    let command = name(&tree, "approvals-connect").unwrap();
    // The program is the CLI built beside the launched binary when there is one, else bare.
    let (program, rest) = command.split_once(" --workspace ").expect(command);
    let launched = Path::new(env!("CARGO_BIN_EXE_dbento-pilot"));
    let sibling = launched.with_file_name("dbento");
    let want = if sibling.is_file() {
        std::fs::canonicalize(&sibling).unwrap_or(sibling)
    } else {
        "dbento".into()
    };
    let got = Path::new(program);
    let got = if got.is_absolute() {
        std::fs::canonicalize(got).unwrap_or_else(|_| got.to_path_buf())
    } else {
        got.to_path_buf()
    };
    assert_eq!(got, want, "{command}");
    assert!(rest.ends_with(" mcp"), "{command}");
    let shot = pilot.call("screenshot", json!({})).await;
    assert!(shot.content.iter().any(|c| c.as_image().is_some()));
}
