//! keymap.json in the running app: an override moves Settings to Ctrl+Alt+S, problems are
//! listed, Reload applies an edit, and (in a session) Open hands the file to xdg-open.
//! Set DBENTO_SHOTS_DIR to keep shots of the Keyboard section in dark and light.
mod common;

use std::os::unix::fs::PermissionsExt;
use std::time::{Duration, Instant};

use common::{Pilot, Rect, bounds, find_node, launch_args, sqlite_workspace};
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
/// The settings body's visible area: the body has no tree node of its own, so it runs from the
/// dialog's top down to the footer's Close button.
fn body(tree: &Value) -> Rect {
    let (dialog, close) = (
        bounds(tree, "settings-dialog"),
        bounds(tree, "settings-close"),
    );
    Rect {
        x: dialog.x,
        y: dialog.y,
        w: dialog.w,
        h: close.y - dialog.y,
    }
}
/// Whether `id` lies wholly inside the settings body's visible area.
fn in_view(tree: &Value, id: &str) -> bool {
    let (body, b) = (body(tree), bounds(tree, id));
    b.y >= body.y - 0.5 && b.y + b.h <= body.y + body.h + 0.5
}
/// Wheels the settings body one notch at a time until `ids` are all in view, so the Keyboard
/// section's head stays on screen instead of flying past to the end of the command list.
async fn scroll_into_view(pilot: &Pilot, ids: &[&str]) {
    for _ in 0..60 {
        let tree = pilot.ok("tree", json!({})).await;
        if ids.iter().all(|id| in_view(&tree, id)) {
            return;
        }
        let body = body(&tree);
        let below = bounds(&tree, ids[0]).y > body.y;
        pilot
            .ok(
                "scroll",
                json!({
                    "target": { "x": body.x + body.w / 2.0, "y": body.y + body.h / 2.0 },
                    "notches": if below { 1 } else { -1 },
                    "settle_ms": 100,
                }),
            )
            .await;
    }
    panic!("{ids:?} never scrolled into view");
}

/// A workspace whose state folder holds `keymap.json` with one good override and two problems,
/// and whose `bin` holds a fake xdg-open that writes its argument to `marker`; both live inside
/// the workspace because the session sandbox sees only the bound workspace argument.
struct Fixture {
    ws: tempfile::TempDir,
    state: std::path::PathBuf,
    keymap: std::path::PathBuf,
    marker: std::path::PathBuf,
    path: String,
}

fn fixture() -> Fixture {
    let ws = sqlite_workspace();
    let (state, bin) = (ws.path().join("state"), ws.path().join("bin"));
    std::fs::create_dir_all(&state).unwrap();
    std::fs::create_dir_all(&bin).unwrap();
    let keymap = state.join("keymap.json");
    std::fs::write(
        &keymap,
        r#"{"app.open_settings": ["ctrl-alt-s"], "nope.x": ["f1"], "tabs.close": "ctrl-w"}"#,
    )
    .unwrap();
    // A fake xdg-open first on PATH records what Open hands it; the real desktop never opens.
    let marker = bin.join("opened");
    let fake = bin.join("xdg-open");
    std::fs::write(
        &fake,
        format!("#!/bin/sh\nprintf '%s' \"$1\" > '{}'\n", marker.display()),
    )
    .unwrap();
    std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = format!("{}:{}", bin.display(), std::env::var("PATH").unwrap());
    Fixture {
        ws,
        state,
        keymap,
        marker,
        path,
    }
}

async fn launch(pilot: &Pilot, mode: &str, f: &Fixture) {
    let mut args = launch_args(mode, f.ws.path());
    args["env"] = json!({ "DBENTO_STATE_DIR": f.state, "PATH": f.path });
    pilot.ok("launch", args).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_keymap_file_moves_keys_lists_problems_and_reloads() {
    let f = fixture();
    let pilot = Pilot::start().await;
    launch(&pilot, "headless", &f).await;

    key(&pilot, "ctrl-,").await;
    pilot.ok("wait", json!({ "ms": 300 })).await;
    let tree = pilot.ok("tree", json!({})).await;
    assert!(
        find_node(&tree, "settings-dialog").is_none(),
        "the old keys still open settings"
    );
    key(&pilot, "ctrl-alt-s").await;
    until(&pilot, "the dialog on the new keys", |t| {
        find_node(t, "settings-dialog").is_some()
    })
    .await;
    shot(&pilot, "settings-top-dark.png").await;
    until(&pilot, "two problems", |t| {
        find_node(t, "settings-keymap-problem-1").is_some()
            && find_node(t, "settings-keymap-ok").is_none()
    })
    .await;
    scroll_into_view(
        &pilot,
        &["settings-keymap-path", "settings-keymap-problem-1"],
    )
    .await;
    shot(&pilot, "keyboard-problems-dark.png").await;

    std::fs::write(&f.keymap, r#"{"app.open_settings": ["ctrl-alt-s"]}"#).unwrap();
    click(&pilot, "settings-keymap-reload").await;
    until(&pilot, "no problems", |t| {
        find_node(t, "settings-keymap-ok").is_some()
    })
    .await;
    click(&pilot, "settings-theme-light").await;
    scroll_into_view(&pilot, &["settings-keymap-path", "settings-keymap-ok"]).await;
    pilot.ok("wait", json!({ "ms": 200 })).await;
    shot(&pilot, "keyboard-ok-light.png").await;
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    pilot.assert_bus_untouched();
}

/// Open needs the real platform: the headless host records open_with_system and never spawns
/// xdg-open. The session tree carries no ids here, so the dialog and button go by role and name.
#[tokio::test(flavor = "multi_thread")]
async fn open_hands_the_keymap_file_to_xdg_open() {
    let f = fixture();
    let pilot = Pilot::start().await;
    launch(&pilot, "session", &f).await;
    let settings = |t: &Value| {
        t["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|n| n["role"] == json!("Dialog") && n["name"] == json!("Settings"))
    };
    key(&pilot, "ctrl-alt-s").await;
    until(&pilot, "the dialog on the new keys", settings).await;
    pilot
        .ok(
            "click",
            json!({ "target": { "role": "Button", "name": "Open keymap file" } }),
        )
        .await;

    // The shell's redirect creates the marker before printf fills it, so wait for the content.
    let deadline = Instant::now() + Duration::from_secs(10);
    let opened = loop {
        match std::fs::read_to_string(&f.marker) {
            Ok(text) if !text.is_empty() => break text,
            _ => assert!(Instant::now() < deadline, "xdg-open was never run"),
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert_eq!(opened, f.keymap.to_string_lossy());
    pilot.ok("close", json!({})).await;
    pilot.assert_bus_untouched();
}
