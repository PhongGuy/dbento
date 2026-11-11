//! Schema objects under gpui-pilot: Enter on a table's index opens its definition in the
//! dock in both themes, and a 1 MiB definition opens and scrolls. Set DBENTO_SHOTS_DIR to
//! keep the shots.
mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use common::{Pilot, bounds, find_node, launch_args, sqlite_workspace};
use serde_json::{Value, json};

/// Runs every statement of `sql` on the workspace's `lite` connection through core.
async fn exec(ws: &Path, sql: &str) {
    let w = dbento_core::Workspace::open(ws).unwrap();
    let conn = w.declared("lite").unwrap();
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

async fn click(pilot: &Pilot, id: &str, extra: Value) {
    let mut args = json!({ "target": { "id": [id] } });
    if let (Some(a), Some(e)) = (args.as_object_mut(), extra.as_object()) {
        a.extend(e.clone());
    }
    pilot.ok("click", args).await;
}

async fn shot(pilot: &Pilot, file: &str) {
    if let Some(dir) = std::env::var_os("DBENTO_SHOTS_DIR").map(std::path::PathBuf::from) {
        std::fs::create_dir_all(&dir).unwrap();
        pilot
            .ok("screenshot", json!({ "save_to": dir.join(file) }))
            .await;
    }
}

const SEED: &str = "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT); \
                    CREATE INDEX t_name ON t(name); INSERT INTO t VALUES (1, 'a');";

#[tokio::test(flavor = "multi_thread")]
async fn enter_on_an_index_shows_its_definition_in_both_themes() {
    for theme in ["dark", "light"] {
        let ws = sqlite_workspace();
        exec(ws.path(), SEED).await;
        let pilot = Pilot::start().await;
        let mut args = launch_args("headless", ws.path());
        args["env"] = json!({ dbento_pilot::THEME_VAR: theme });
        pilot.ok("launch", args).await;
        click(&pilot, "connection-toggle-lite", json!({})).await;
        until(&pilot, "the table row", |t| {
            name(t, "tree-row-2") == Some("lite.main.t")
        })
        .await;
        click(&pilot, "tree-toggle-2", json!({})).await;
        until(&pilot, "t's groups", |t| {
            name(t, "tree-row-5") == Some("lite.main.t/keys")
                && name(t, "tree-row-6") == Some("lite.main.t/indexes")
        })
        .await;
        click(&pilot, "tree-toggle-6", json!({})).await;
        until(&pilot, "the index row", |t| {
            name(t, "tree-row-7") == Some("lite.main.t/indexes.t_name")
        })
        .await;
        click(&pilot, "tree-row-7", json!({})).await;
        pilot.ok("key", json!({ "keys": "enter" })).await;
        let tree = until(&pilot, "the definition", |t| {
            name(t, "definition-editor") == Some("CREATE INDEX t_name ON t(name);")
        })
        .await;
        assert_eq!(
            name(&tree, "definition-title"),
            Some("Index t_name on main.t")
        );
        for id in [
            "definition-panel",
            "definition-reload",
            "definition-copy",
            "definition-open-tab",
            "definition-close",
        ] {
            assert!(find_node(&tree, id).is_some(), "{theme}: no {id}");
        }
        assert!(
            find_node(&tree, "definition-note").is_none(),
            "{theme}: engine text has no note"
        );
        shot(&pilot, &format!("schema-objects-{theme}.png")).await;
        click(&pilot, "definition-close", json!({})).await;
        until(&pilot, "the dock closed", |t| {
            find_node(t, "definition-panel").is_none()
        })
        .await;
        assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
        pilot.assert_bus_untouched();
    }
}

/// A view of about 1 MiB: one CASE branch per line.
fn big_view() -> String {
    let mut sql = String::from("CREATE VIEW big AS SELECT CASE 0\n");
    let mut i = 0;
    while sql.len() < (1 << 20) - 64 {
        sql.push_str(&format!("  WHEN {i} THEN 'value {i:08}'\n"));
        i += 1;
    }
    sql.push_str("END AS c;");
    sql
}

// Prints the open time and the frames report for the cap probe note.
#[tokio::test(flavor = "multi_thread")]
async fn a_one_mib_view_definition_opens_and_scrolls() {
    let ws = sqlite_workspace();
    exec(ws.path(), &big_view()).await;
    let pilot = Pilot::start().await;
    pilot.ok("launch", launch_args("headless", ws.path())).await;
    click(&pilot, "connection-toggle-lite", json!({})).await;
    until(&pilot, "the view row", |t| {
        name(t, "tree-row-2") == Some("lite.main.big")
    })
    .await;
    click(&pilot, "tree-row-2", json!({ "button": "right" })).await;
    until(&pilot, "the menu", |t| {
        find_node(t, "tree-menu-view-definition").is_some()
    })
    .await;
    let opened = Instant::now();
    click(&pilot, "tree-menu-view-definition", json!({})).await;
    let tree = until(&pilot, "the big definition", |t| {
        name(t, "definition-editor")
            .is_some_and(|n| n.starts_with("CREATE VIEW big AS SELECT CASE 0"))
    })
    .await;
    let open_ms = opened.elapsed().as_millis();
    let editor = bounds(&tree, "definition-editor");
    let at = json!({ "x": editor.x + editor.w / 2.0, "y": editor.y + editor.h / 2.0 });
    pilot.ok("frames", json!({ "action": "start" })).await;
    pilot
        .ok(
            "scroll",
            json!({ "target": at, "notches": 200, "notch_ms": 30, "settle_ms": 0 }),
        )
        .await;
    pilot.ok("wait", json!({ "ms": 150 })).await;
    let report = pilot.ok("frames", json!({ "action": "stop" })).await;
    shot(&pilot, "schema-objects-scrolled.png").await;
    eprintln!("one-MiB definition: menu click to editor {open_ms} ms\nframes stop: {report}");
    assert!(report["frames"].as_u64().unwrap() >= 50, "{report}");
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    pilot.assert_bus_untouched();
}
