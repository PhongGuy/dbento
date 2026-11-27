//! Settings in the running app: Ctrl+, opens it, Compact and a larger grid font change the
//! row heights live, and a relaunch on the same state folder restores both.
//! Set DBENTO_SHOTS_DIR to keep shots of the dialog in dark, Compact, at UI size 16 and in light.
mod common;

use std::time::{Duration, Instant};

use common::{Pilot, bounds, find_node, launch_args, run_query, session_app, sqlite_workspace};
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

/// The text the size field `id` shows: its editor's value in the tree.
fn shown<'a>(tree: &'a Value, id: &str) -> Option<&'a str> {
    tree["nodes"].as_array()?.iter().find_map(|n| {
        let inside = n["path"].as_array()?.contains(&json!(id));
        (inside && n["role"] == json!("TextInput")).then(|| n["value"].as_str())?
    })
}

// A step and then a click into the size field each move focus to it once; with a11y on, a
// field that reported focus twice per frame aborted the app. The field then still types and
// steps from the keyboard.
#[tokio::test(flavor = "multi_thread")]
async fn stepping_a_size_and_clicking_into_it_keeps_the_field_working() {
    let ws = sqlite_workspace();
    let pilot = Pilot::start().await;
    pilot.ok("launch", launch_args("headless", ws.path())).await;
    pilot
        .ok("tree", json!({ "wait_for": { "id": ["shell"] } }))
        .await;
    key(&pilot, "ctrl-,").await;
    until(&pilot, "the dialog", |t| {
        find_node(t, "settings-dialog").is_some()
    })
    .await;
    let field = "settings-grid-font-size";
    increment(&pilot, field, 1).await;
    click(&pilot, field).await;
    until(&pilot, "the stepped size", |t| {
        shown(t, field) == Some("13")
    })
    .await;
    key(&pilot, "ctrl-a").await;
    pilot.ok("type", json!({ "text": "15" })).await;
    key(&pilot, "enter").await;
    let tree = until(&pilot, "the typed size", |t| shown(t, field) == Some("15")).await;
    find_node(&tree, "settings-dialog").expect("Enter keeps the dialog open");
    key(&pilot, "up").await;
    until(&pilot, "the size Up stepped", |t| {
        shown(t, field) == Some("16")
    })
    .await;
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    pilot.assert_bus_untouched();
}

// The same step and click under the session's accessibility tree, which reports focus but
// few ids: one node holds the focus, and it sits inside the size field.
#[tokio::test(flavor = "multi_thread")]
async fn a_stepped_and_clicked_size_field_is_the_one_focused_node() {
    let app = session_app();
    assert!(app.is_file(), "no optimised app at {}", app.display());
    let ws = sqlite_workspace();
    let pilot = Pilot::start().await;
    let mut args = launch_args("session", ws.path());
    args["env"] = json!({ "DBENTO_STATE_DIR": ws.path().join(".state") });
    pilot.ok("launch", args).await;
    pilot
        .ok("tree", json!({ "wait_for": { "id": ["title-search"] } }))
        .await;
    key(&pilot, "ctrl-,").await;
    let spin = |n: &Value| n["role"] == json!("SpinButton") && n["name"] == json!("Grid font size");
    let tree = until(&pilot, "the size field", |t| {
        t["nodes"].as_array().is_some_and(|n| n.iter().any(spin))
    })
    .await;
    let field = tree["nodes"]
        .as_array()
        .and_then(|n| n.iter().find(|n| spin(n)))
        .expect("the size field")
        .clone();
    // The increment button is the field's right end; the click then lands mid-field.
    let b = &field["bounds"];
    let at = |k: &str| b[k].as_f64().expect("bounds");
    let (right, mid) = (at("x") + at("w") - 10., at("y") + at("h") / 2.);
    pilot
        .ok("click", json!({ "target": { "x": right, "y": mid } }))
        .await;
    pilot
        .ok(
            "click",
            json!({ "target": { "role": "SpinButton", "name": "Grid font size" } }),
        )
        .await;
    // Wait for focus to reach the field, then require it to be the only focused node.
    let outer = field["path"].as_array().expect("a path").clone();
    let inside = |n: &Value| {
        n["path"]
            .as_array()
            .is_some_and(|p| p.len() > outer.len() && p.starts_with(&outer))
    };
    let tree = until(&pilot, "focus inside the size field", |t| {
        t["nodes"]
            .as_array()
            .is_some_and(|n| n.iter().any(|n| n["focused"] == json!(true) && inside(n)))
    })
    .await;
    let focused: Vec<&Value> = tree["nodes"]
        .as_array()
        .expect("tree nodes")
        .iter()
        .filter(|n| n["focused"] == json!(true))
        .collect();
    assert_eq!(focused.len(), 1, "one focused node: {focused:?}");
    // The click on the right end stepped 12 to 13, as the saved file shows.
    let file = ws.path().join(".state/settings.json");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let read = std::fs::read_to_string(&file).ok();
        let saved = read
            .as_deref()
            .and_then(|t| serde_json::from_str::<Value>(t).ok());
        if saved.is_some_and(|s| s["grid_font"]["size"] == json!(13)) {
            break;
        }
        assert!(Instant::now() < deadline, "the step never saved: {read:?}");
        pilot.ok("wait", json!({ "ms": 50 })).await;
    }
    pilot.ok("close", json!({})).await;
    pilot.assert_bus_untouched();
}

/// The bounds of every option the open UI family list shows, in list order.
fn option_rows(tree: &Value) -> Vec<common::Rect> {
    (0..)
        .map(|n| format!("settings-ui-font-option-{n}"))
        .take_while(|id| find_node(tree, id).is_some())
        .map(|id| bounds(tree, &id))
        .collect()
}

// The family list holds more rows than its 220 px fit; each row keeps its 22 px and the list
// scrolls, at the smallest window and a large one. The host offers the machine's installed
// families, so the list is full on any desktop with a dozen fonts.
#[tokio::test(flavor = "multi_thread")]
async fn the_family_list_keeps_full_height_rows_and_scrolls() {
    for (width, height) in [(480., 320.), (1280., 800.), (1920., 1200.)] {
        let (ws, state) = (sqlite_workspace(), tempfile::tempdir().unwrap());
        let pilot = Pilot::start().await;
        let mut args = launch_args("headless", ws.path());
        args["width"] = json!(width);
        args["height"] = json!(height);
        args["env"] = json!({ "DBENTO_STATE_DIR": state.path() });
        pilot.ok("launch", args).await;
        key(&pilot, "ctrl-,").await;
        until(&pilot, "the dialog", |t| {
            find_node(t, "settings-ui-font-family").is_some()
        })
        .await;
        click(&pilot, "settings-ui-font-family").await;
        until(&pilot, "a family list of 12 (it needs a dozen installed fonts)", |t| {
            find_node(t, "settings-ui-font-option-11").is_some()
        })
        .await;
        pilot.ok("wait", json!({ "ms": 200 })).await;
        let tree = pilot.ok("tree", json!({})).await;
        shot(&pilot, &format!("settings-font-list-{width}x{height}.png")).await;

        let size = format!("{width}x{height}");
        let rows = option_rows(&tree);
        for (n, r) in rows.iter().enumerate() {
            assert!(
                (r.h - 22.).abs() < 0.5,
                "{size}: option {n} is {r:?}, not 22 px tall"
            );
        }
        for (n, pair) in rows.windows(2).enumerate() {
            assert!(
                pair[0].y + pair[0].h <= pair[1].y + 0.5,
                "{size}: option {n} {:?} overlaps the next {:?}",
                pair[0],
                pair[1]
            );
        }
        let list = bounds(&tree, "settings-ui-font-list");
        let (first, last) = (rows[0], rows[rows.len() - 1]);
        assert!(
            last.y + last.h > list.y + list.h,
            "{size}: the rows end at {} inside the list {list:?}, so nothing scrolls",
            last.y + last.h
        );

        // In the large windows the list is on screen, and wheeling over it brings its last
        // option into view; the smallest window leaves it below the fold.
        if height >= 800. {
            assert!(
                list.y + list.h <= height,
                "{size}: the list {list:?} is off screen"
            );
            pilot
                .ok(
                    "scroll",
                    json!({ "target": { "x": list.x + list.w / 2.0, "y": list.y + list.h / 2.0 }, "notches": 200, "settle_ms": 300 }),
                )
                .await;
            let tree = pilot.ok("tree", json!({})).await;
            let rows = option_rows(&tree);
            let last = rows[rows.len() - 1];
            assert!(
                last.y + last.h <= list.y + list.h + 1.0 && rows[0].y < first.y,
                "{size}: the last option {last:?} never scrolled into the list {list:?}"
            );
        }
        pilot.ok("close", json!({})).await;
        pilot.assert_bus_untouched();
    }
}
