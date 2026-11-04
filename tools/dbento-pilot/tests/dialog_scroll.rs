//! The connection dialog in a short window: its body scrolls, its footer stays in view,
//! and Host and Port share a row. Set DBENTO_SHOTS_DIR to keep the shots.
mod common;

use std::time::{Duration, Instant};

use common::{Pilot, bounds, find_node, launch_args, sqlite_workspace};
use serde_json::{Value, json};

/// Short enough that the Postgres form (the taller engine) cannot fit under its title.
const HEIGHT: f64 = 480.;

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

#[tokio::test(flavor = "multi_thread")]
async fn a_short_window_scrolls_the_body_and_keeps_the_footer() {
    let ws = sqlite_workspace();
    let pilot = Pilot::start().await;
    let mut args = launch_args("headless", ws.path());
    args["height"] = json!(HEIGHT);
    pilot.ok("launch", args).await;
    pilot
        .ok("click", json!({ "target": { "id": ["connection-new"] } }))
        .await;
    let tree = until(&pilot, "the dialog", |t| {
        find_node(t, "conn-save").is_some()
    })
    .await;
    pilot.ok("wait", json!({ "ms": 300 })).await;
    shot(&pilot, "dialog-short.png").await;

    for id in ["conn-test", "conn-cancel", "conn-save"] {
        let b = bounds(&tree, id);
        assert!(
            b.y + b.h <= HEIGHT,
            "{id} ends at {} past the {HEIGHT} px window",
            b.y + b.h
        );
    }
    let (host, port) = (bounds(&tree, "conn-host"), bounds(&tree, "conn-port"));
    assert!(
        (host.y - port.y).abs() <= 1.0 && port.x > host.x + host.w,
        "Host {host:?} and Port {port:?} are not on one row"
    );

    // Wheeling over the body brings the last field up into the window.
    let body = bounds(&tree, "conn-body");
    let folder = bounds(&tree, "conn-folder");
    assert!(folder.y + folder.h > body.y + body.h, "Folder already fits");
    pilot
        .ok(
            "scroll",
            json!({ "target": { "x": body.x + body.w / 2.0, "y": body.y + body.h / 2.0 }, "notches": 20, "settle_ms": 300 }),
        )
        .await;
    let tree = pilot.ok("tree", json!({})).await;
    let folder = bounds(&tree, "conn-folder");
    assert!(
        folder.y + folder.h <= body.y + body.h + 1.0,
        "Folder {folder:?} is still below the body {body:?}"
    );
    shot(&pilot, "dialog-short-scrolled.png").await;

    // The footer still answers: Cancel closes the dialog.
    pilot
        .ok("click", json!({ "target": { "id": ["conn-cancel"] } }))
        .await;
    until(&pilot, "the dialog to close", |t| {
        find_node(t, "conn-dialog").is_none()
    })
    .await;
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    pilot.assert_bus_untouched();
}

/// A window tall enough for the form shows it whole, with the footer right under the body
/// rather than stretched apart from it. The new Postgres form needs a little over 800 px.
#[tokio::test(flavor = "multi_thread")]
async fn a_tall_window_shows_the_whole_form() {
    let ws = sqlite_workspace();
    let pilot = Pilot::start().await;
    let mut args = launch_args("headless", ws.path());
    args["height"] = json!(900);
    pilot.ok("launch", args).await;
    pilot
        .ok("click", json!({ "target": { "id": ["connection-new"] } }))
        .await;
    let tree = until(&pilot, "the dialog", |t| {
        find_node(t, "conn-save").is_some()
    })
    .await;
    pilot.ok("wait", json!({ "ms": 300 })).await;
    shot(&pilot, "dialog-tall.png").await;
    let (body, folder, save) = (
        bounds(&tree, "conn-body"),
        bounds(&tree, "conn-folder"),
        bounds(&tree, "conn-save"),
    );
    assert!(
        folder.y + folder.h <= body.y + body.h,
        "Folder {folder:?} is cut off by the body {body:?}"
    );
    assert!(
        save.y - (body.y + body.h) <= 24.0,
        "the footer sits apart from the body: Save {save:?}, body {body:?}"
    );
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
}
