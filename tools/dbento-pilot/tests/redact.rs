//! Secrets typed into the connection dialog never reach the pilot's replies: the masked
//! password field is redacted on its own, and `redact: true` covers a plain field too.
mod common;

use std::time::{Duration, Instant};

use common::{Pilot, find_node, launch_args, sqlite_workspace};
use rmcp::model::CallToolResult;
use serde_json::{Value, json};

/// Every three-character run of `secret` that appears in `log`, as gpui-pilot's own drive
/// log test checks it.
fn leaks(log: &str, secret: &str) -> Vec<String> {
    let chars: Vec<char> = secret.chars().collect();
    chars
        .windows(3)
        .map(|w| w.iter().collect::<String>())
        .filter(|part| log.contains(part.as_str()))
        .collect()
}

/// Calls `tool` and appends its whole result, structured and text, to `log`.
async fn logged(pilot: &Pilot, log: &mut String, tool: &str, args: Value) -> Value {
    let result: CallToolResult = pilot.call(tool, args).await;
    log.push_str(&serde_json::to_string(&result).unwrap());
    log.push('\n');
    result.structured_content.expect("structured content")
}

async fn until_node(pilot: &Pilot, log: &mut String, id: &str) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let tree = logged(pilot, log, "tree", json!({})).await;
        if find_node(&tree, id).is_some() {
            return;
        }
        assert!(Instant::now() < deadline, "{id} never appeared: {tree}");
        pilot.ok("wait", json!({ "ms": 50 })).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn typed_secrets_never_reach_the_replies() {
    // Unusual characters, so a three-character run cannot turn up in a tree by chance.
    let (asked, auto, plain) = ("Qz7#vK9!xW", "Jq4%Xb8&Zp", "Vy3@Wk6^Hj");
    let ws = sqlite_workspace();
    let pilot = Pilot::start().await;
    let mut log = String::new();
    logged(
        &pilot,
        &mut log,
        "launch",
        launch_args("headless", ws.path()),
    )
    .await;
    let new = json!({ "target": { "id": ["connection-new"] } });
    logged(&pilot, &mut log, "click", new).await;
    until_node(&pilot, &mut log, "conn-password").await;

    let password = json!({ "id": ["conn-password"] });
    let typed = logged(
        &pilot,
        &mut log,
        "type",
        json!({ "text": asked, "target": password, "redact": true }),
    )
    .await;
    assert_eq!(typed["redacted"], json!("<redacted 10 chars>"), "{typed}");
    assert_eq!(typed["masked"], json!(true), "{typed}");

    // Without `redact`, the host sees the masked input and redacts by itself.
    let typed = logged(
        &pilot,
        &mut log,
        "type",
        json!({ "text": auto, "target": password }),
    )
    .await;
    assert_eq!(typed["redacted"], json!("<redacted 10 chars>"), "{typed}");
    assert_eq!(typed["masked"], json!(true), "{typed}");

    // A plain field shows its value in `tree`; the server scrubs a redacted text from it.
    let name = json!({ "id": ["conn-name"] });
    let typed = logged(
        &pilot,
        &mut log,
        "type",
        json!({ "text": plain, "target": name, "redact": true }),
    )
    .await;
    assert_eq!(typed["redacted"], json!("<redacted 10 chars>"), "{typed}");
    logged(&pilot, &mut log, "tree", json!({})).await;

    for secret in [asked, auto, plain] {
        assert_eq!(leaks(&log, secret), Vec::<String>::new(), "{log}");
    }
    assert!(log.contains("<redacted 10 chars>"), "{log}");
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    pilot.assert_bus_untouched();
}
