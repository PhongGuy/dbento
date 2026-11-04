//! uxnits group 01 under gpui-pilot: Stop takes a reserved toolbar slot, a sorted narrow
//! header keeps its type label, and the filter's clear X shows only with text. Set
//! DBENTO_SHOTS_DIR to keep the shots.
mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::{Pilot, Rect, bounds, find_node, launch_args, sqlite_workspace};
use serde_json::{Value, json};

/// A statement SQLite takes many seconds over, so Stop stays up while the test looks.
const SLOW: &str = "WITH RECURSIVE r(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM r \
    WHERE x < 1000000000) SELECT count(*) FROM r";

async fn exec(ws: &Path, sql: &str) {
    let w = dbento_core::Workspace::open(ws).unwrap();
    let conn = w.connection("lite").unwrap();
    let mut s = dbento_core::connect(&w, conn, None, Default::default())
        .await
        .unwrap();
    {
        let mut script = s.script(sql);
        while let Some(p) = script.next().await {
            let mut stream = p.unwrap().execute(Default::default()).await.unwrap();
            while stream.next_batch().await.unwrap().is_some() {}
        }
    }
    s.close().await.unwrap();
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

async fn click(pilot: &Pilot, id: &str, count: u32) {
    pilot
        .ok("click", json!({ "target": { "id": [id] }, "count": count }))
        .await;
}

fn name<'a>(tree: &'a Value, id: &str) -> Option<&'a str> {
    find_node(tree, id).and_then(|n| n["name"].as_str())
}

fn spot(r: Rect) -> (f64, f64, f64, f64) {
    (r.x, r.y, r.w, r.h)
}

async fn shot(pilot: &Pilot, dir: &Path, id: &str, file: String) {
    pilot
        .ok(
            "screenshot",
            json!({ "target": { "id": [id] }, "save_to": dir.join(file) }),
        )
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn toolbar_slot_header_room_and_filter_clear() {
    let temp = tempfile::tempdir().unwrap();
    let dir: PathBuf = std::env::var_os("DBENTO_SHOTS_DIR").map_or(temp.path().into(), Into::into);
    std::fs::create_dir_all(&dir).unwrap();
    for theme in ["dark", "light"] {
        let ws = sqlite_workspace();
        exec(
            ws.path(),
            "CREATE TABLE n(v INTEGER, w TEXT); INSERT INTO n VALUES (1, 'a'), (2, 'b'), (3, 'c');",
        )
        .await;
        let pilot = Pilot::start().await;
        let mut args = launch_args("headless", ws.path());
        args["env"] = json!({ dbento_pilot::THEME_VAR: theme });
        pilot.ok("launch", args).await;

        // A browse of the table: the filter is empty, so no clear X yet.
        click(&pilot, "connection-toggle-lite", 1).await;
        until(&pilot, "the table row", |t| {
            name(t, "tree-row-2") == Some("lite.main.n")
        })
        .await;
        click(&pilot, "tree-row-2", 2).await;
        let tree = until(&pilot, "three rows", |t| {
            name(t, "footer-rows") == Some("3 rows")
        })
        .await;
        assert!(find_node(&tree, "browse-filter").is_some(), "{tree}");
        assert!(find_node(&tree, "browse-filter-clear").is_none(), "{tree}");
        shot(
            &pilot,
            &dir,
            "browse-filter",
            format!("filter-empty-{theme}.png"),
        )
        .await;

        // Sorting the narrow first column descending draws its arrow in room kept for it.
        click(&pilot, "result-col-0", 1).await;
        until(&pilot, "ascending", |t| {
            name(t, "result-sort-0") == Some("ascending")
        })
        .await;
        click(&pilot, "result-col-0", 1).await;
        until(&pilot, "descending", |t| {
            name(t, "result-sort-0") == Some("descending")
        })
        .await;
        pilot
            .ok("hover", json!({ "target": { "id": ["footer-rows"] } }))
            .await;
        pilot.ok("wait", json!({ "ms": 300 })).await;
        shot(
            &pilot,
            &dir,
            "result-grid",
            format!("header-sorted-{theme}.png"),
        )
        .await;

        pilot
            .ok(
                "type",
                json!({ "target": { "id": ["browse-filter-input"] }, "text": "v > 1" }),
            )
            .await;
        until(&pilot, "the clear X", |t| {
            find_node(t, "browse-filter-clear").is_some()
        })
        .await;
        shot(
            &pilot,
            &dir,
            "browse-filter",
            format!("filter-text-{theme}.png"),
        )
        .await;

        assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));

        // Stop appears in a slot held for it, so Save and the row limit stay put. A fresh
        // workspace restores no browse tab, so the double click opens a query tab.
        let ws = sqlite_workspace();
        let pilot = Pilot::start().await;
        let mut args = launch_args("headless", ws.path());
        args["env"] = json!({ dbento_pilot::THEME_VAR: theme });
        pilot.ok("launch", args).await;
        click(&pilot, "connection-lite", 2).await;
        pilot
            .ok("tree", json!({ "wait_for": { "id": ["editor"] } }))
            .await;
        pilot
            .ok(
                "type",
                json!({ "target": { "id": ["editor"] }, "text": SLOW }),
            )
            .await;
        let idle = pilot.ok("tree", json!({})).await;
        assert!(find_node(&idle, "cancel").is_none());
        shot(
            &pilot,
            &dir,
            "query-toolbar",
            format!("toolbar-idle-{theme}.png"),
        )
        .await;
        click(&pilot, "run-all", 1).await;
        let running = until(&pilot, "the Stop button", |t| {
            find_node(t, "cancel").is_some()
        })
        .await;
        for id in ["query-save", "row-limit"] {
            assert_eq!(spot(bounds(&running, id)), spot(bounds(&idle, id)), "{id}");
        }
        shot(
            &pilot,
            &dir,
            "query-toolbar",
            format!("toolbar-running-{theme}.png"),
        )
        .await;
        click(&pilot, "cancel", 1).await;
        until(&pilot, "the stop", |t| find_node(t, "cancel").is_none()).await;
        assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    }
}
