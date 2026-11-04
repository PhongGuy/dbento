//! Dbento driven end to end over MCP: a `gpui-pilot` server in a scratch project launches
//! this package's binary headless on a fixture workspace.

mod common;

use std::path::PathBuf;

use common::Pilot;
use rmcp::model::CallToolRequestParams;
use serde_json::json;

#[tokio::test(flavor = "multi_thread")]
async fn the_app_opens_headless_and_closes_cleanly() {
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("dbento.toml"), "dbento = 1\n").unwrap();
    let pilot = Pilot::start().await;
    let launch = pilot
        .ok(
            "launch",
            json!({ "mode": "headless", "build": false, "args": [ws.path()] }),
        )
        .await;
    let data_dir = PathBuf::from(launch["data_dir"].as_str().unwrap());
    pilot
        .ok("tree", json!({ "wait_for": { "id": ["shell"] } }))
        .await;
    let shot = pilot.call("screenshot", json!({})).await;
    assert!(shot.content.iter().any(|c| c.as_image().is_some()));
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    assert!(!data_dir.exists());
    pilot.assert_bus_untouched();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_relative_workspace_fails_the_launch() {
    let pilot = Pilot::start().await;
    let result = pilot
        .client
        .call_tool(
            CallToolRequestParams::new("launch".to_owned()).with_arguments(
                json!({ "mode": "headless", "build": false, "args": ["ws"] })
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        )
        .await
        .unwrap();
    assert_eq!(result.is_error, Some(true));
    let error = result.structured_content.unwrap();
    assert_eq!(error["code"], json!("LaunchFailed"));
    assert!(
        error["message"]
            .as_str()
            .unwrap()
            .contains("must be absolute"),
        "{error}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn headless_frames_come_from_an_optimised_host() {
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("dbento.toml"), "dbento = 1\n").unwrap();
    let pilot = Pilot::start().await;
    pilot
        .ok(
            "launch",
            json!({ "mode": "headless", "build": false, "args": [ws.path()] }),
        )
        .await;
    pilot
        .ok("tree", json!({ "wait_for": { "id": ["shell"] } }))
        .await;
    let started = pilot.ok("frames", json!({ "action": "start" })).await;
    assert!(
        started["refresh_mhz"].as_u64().unwrap() >= 143_000,
        "the default 144 Hz: {started}"
    );
    pilot.ok("wait", json!({ "ms": 200 })).await;
    let report = pilot.ok("frames", json!({ "action": "stop" })).await;
    assert_eq!(report["build"]["opt_level"], json!("3"), "{report}");
    assert!(
        !report["warnings"]
            .as_array()
            .unwrap()
            .contains(&json!("unoptimised_host")),
        "{report}"
    );
    pilot.ok("close", json!({})).await;
    pilot.assert_bus_untouched();
}
