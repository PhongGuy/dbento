//! QA under gpui-pilot: outside edits to dbento.toml and a saved query reach the window.
mod common;

use std::time::{Duration, Instant};

use common::{Pilot, find_node, launch_args, node, sqlite_workspace};
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

// The editor's whole text, copied through the clipboard; the caret ends at the end.
async fn editor_text(pilot: &Pilot) -> String {
    click(pilot, "editor").await;
    for keys in ["ctrl-a", "ctrl-c", "ctrl-end"] {
        key(pilot, keys).await;
    }
    let clip = pilot.ok("clipboard", json!({})).await;
    clip["text"].as_str().unwrap_or_default().to_owned()
}

async fn shot(pilot: &Pilot, name: &str) {
    if let Some(dir) = std::env::var_os("DBENTO_SHOTS_DIR").map(std::path::PathBuf::from) {
        std::fs::create_dir_all(&dir).unwrap();
        pilot
            .ok("screenshot", json!({ "save_to": dir.join(name) }))
            .await;
    }
}

async fn resize(pilot: &Pilot, width: u32) {
    pilot
        .ok("resize", json!({ "width": width, "height": 800 }))
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn outside_edits_reach_the_window() {
    let ws = sqlite_workspace();
    let toml = ws.path().join("dbento.toml");
    let file = ws.path().join("queries/lite/a.sql");
    std::fs::create_dir_all(file.parent().unwrap()).unwrap();
    std::fs::write(&file, "SELECT 1").unwrap();
    let pilot = Pilot::start().await;
    pilot.ok("launch", launch_args("headless", ws.path())).await;
    common::open_connection(&pilot, "connection-lite").await;
    until(&pilot, "the editor", |t| find_node(t, "editor").is_some()).await;

    let base = std::fs::read_to_string(&toml).unwrap();
    std::fs::write(
        &toml,
        format!("{base}\n[connections.extra]\nengine = \"sqlite\"\npath = \"app.db\"\n"),
    )
    .unwrap();
    until(&pilot, "the new connection", |t| {
        find_node(t, "connection-extra").is_some()
    })
    .await;
    shot(&pilot, "sidebar-reloaded.png").await;

    click(&pilot, "editor").await;
    key(&pilot, "ctrl-o").await;
    until(&pilot, "the palette", |t| find_node(t, "palette").is_some()).await;
    pilot.ok("wait", json!({ "ms": 300 })).await;
    key(&pilot, "enter").await;
    until(&pilot, "the opened file", |t| {
        find_node(t, "palette").is_none()
    })
    .await;
    std::fs::write(&file, "SELECT 2").unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    while editor_text(&pilot).await != "SELECT 2" {
        assert!(
            Instant::now() < deadline,
            "the clean tab never took the new text"
        );
        pilot.ok("wait", json!({ "ms": 100 })).await;
    }
    shot(&pilot, "tab-reloaded.png").await;

    pilot
        .ok(
            "type",
            json!({ "target": { "id": ["editor"] }, "text": " -- mine" }),
        )
        .await;
    std::fs::write(&file, "SELECT 3").unwrap();
    until(&pilot, "the disk banner", |t| {
        find_node(t, "disk-banner").is_some()
    })
    .await;
    shot(&pilot, "disk-banner.png").await;
    resize(&pilot, 560).await;
    shot(&pilot, "disk-banner-narrow.png").await;
    resize(&pilot, 1280).await;

    // Compare shows the diff in a dialog; Cancel leaves the banner as it was.
    click(&pilot, "disk-compare").await;
    until(&pilot, "the compare dialog", |t| {
        find_node(t, "disk-compare-cancel").is_some()
    })
    .await;
    shot(&pilot, "compare-dialog.png").await;
    click(&pilot, "disk-compare-cancel").await;
    until(&pilot, "the dialog to go", |t| {
        find_node(t, "disk-compare-cancel").is_none() && find_node(t, "disk-banner").is_some()
    })
    .await;

    click(&pilot, "disk-keep").await;
    until(&pilot, "the banner to go", |t| {
        find_node(t, "disk-banner").is_none()
    })
    .await;

    std::fs::write(&toml, format!("{base}read_only = true\n")).unwrap();
    let tree = until(&pilot, "the fence chip", |t| {
        find_node(t, "conn-chip").is_some()
    })
    .await;
    assert_eq!(
        node(&tree, "conn-chip")["name"],
        json!("Now read-only: Reconnect to run (rolls back any open transaction)")
    );
    shot(&pilot, "fence-chip.png").await;

    std::fs::write(&toml, "dbento = 1\n[connections.lite]\nengine = \"nope\"\n").unwrap();
    until(&pilot, "the workspace banner", |t| {
        find_node(t, "workspace-banner").is_some()
    })
    .await;
    shot(&pilot, "workspace-banner.png").await;
    resize(&pilot, 560).await;
    shot(&pilot, "workspace-banner-narrow.png").await;

    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
}
