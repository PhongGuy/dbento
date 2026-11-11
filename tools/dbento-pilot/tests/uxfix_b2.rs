//! uxfix B2 under gpui-pilot: a staged cell's original in the status bar, the conflict
//! banner's drop action, and a committed row kept after it left the filter.
mod common;

use std::path::Path;
use std::time::{Duration, Instant};

use common::{Pilot, find_node, launch_args, sqlite_workspace};
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

async fn click(pilot: &Pilot, id: &str) {
    pilot.ok("click", json!({ "target": { "id": [id] } })).await;
}

async fn key(pilot: &Pilot, keys: &str) {
    pilot.ok("key", json!({ "keys": keys })).await;
}

/// Opens cell (row, 1) of the grid, types `text` over it and stages it with Enter.
async fn edit(pilot: &Pilot, row: usize, text: &str) {
    click(pilot, &format!("cell-{row}-1")).await;
    key(pilot, "enter").await;
    until(pilot, "the cell editor", |t| {
        find_node(t, "cell-editor").is_some()
    })
    .await;
    key(pilot, "ctrl-a").await;
    pilot.ok("type", json!({ "text": text })).await;
    key(pilot, "enter").await;
}

async fn save_and_commit(pilot: &Pilot) {
    click(pilot, "save-edits").await;
    until(pilot, "the prompt", |t| {
        find_node(t, "apply-commit").is_some()
    })
    .await;
    click(pilot, "apply-commit").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn original_value_conflict_drop_and_kept_rows() {
    let ws = sqlite_workspace();
    exec(
        ws.path(),
        "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT); \
         INSERT INTO t VALUES (1, 'b'), (2, 'a'), (3, 'c');",
    )
    .await;
    let pilot = Pilot::start().await;
    pilot.ok("launch", launch_args("headless", ws.path())).await;
    click(&pilot, "connection-toggle-lite").await;
    until(&pilot, "the table row", |t| {
        name(t, "tree-row-2") == Some("lite.main.t")
    })
    .await;
    pilot
        .ok(
            "click",
            json!({ "target": { "id": ["tree-row-2"] }, "count": 2 }),
        )
        .await;
    until(&pilot, "three rows", |t| {
        name(t, "footer-rows") == Some("3 rows")
    })
    .await;

    // A second click on the selected cell selects its whole row, so the selection leaves
    // the staged cell and comes back to show that the status bar follows it.
    edit(&pilot, 0, "z").await;
    click(&pilot, "cell-1-1").await;
    until(&pilot, "an unstaged cell", |t| {
        find_node(t, "status-was").is_none()
    })
    .await;
    click(&pilot, "cell-0-1").await;
    until(&pilot, "the original", |t| {
        name(t, "status-was") == Some("Was: b")
    })
    .await;

    edit(&pilot, 1, "y").await;
    exec(ws.path(), "DELETE FROM t WHERE id = 1").await;
    click(&pilot, "save-edits").await;
    until(&pilot, "the conflict actions", |t| {
        find_node(t, "edit-banner-drop").is_some()
    })
    .await;
    click(&pilot, "edit-banner-drop").await;
    until(&pilot, "the prompt", |t| {
        find_node(t, "apply-commit").is_some()
    })
    .await;
    click(&pilot, "apply-commit").await;
    until(&pilot, "two rows", |t| {
        name(t, "footer-rows") == Some("2 rows")
    })
    .await;

    pilot
        .ok(
            "type",
            json!({ "target": { "id": ["browse-filter-input"] }, "text": "name <> 'k'" }),
        )
        .await;
    key(&pilot, "enter").await;
    edit(&pilot, 0, "k").await;
    save_and_commit(&pilot).await;
    until(&pilot, "the kept row", |t| {
        name(t, "kept-row-0") == Some("id = 2 \u{b7} name: k \u{b7} no longer matches filter")
    })
    .await;
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    pilot.assert_bus_untouched();
}

/// One RGB pixel of a PNG at logical (x, y), at the launch's scale of 1.
fn pixel(png_path: &Path, x: f64, y: f64) -> [u8; 3] {
    let bytes = std::fs::read(png_path).unwrap();
    let mut reader = png::Decoder::new(std::io::Cursor::new(bytes))
        .read_info()
        .unwrap();
    let mut buf = vec![0; reader.output_buffer_size().unwrap()];
    let frame = reader.next_frame(&mut buf).unwrap();
    let step = frame.line_size / frame.width as usize;
    let at = y as usize * frame.line_size + x as usize * step;
    [buf[at], buf[at + 1], buf[at + 2]]
}

/// The conflict banner's actions carry a fill or border of their own in both themes, so
/// they read as buttons: the pixel on each button's left edge differs from the
/// banner beside it. Set DBENTO_SHOTS_DIR to keep the banner shots.
#[tokio::test(flavor = "multi_thread")]
async fn conflict_banner_actions_read_as_buttons() {
    let shots = tempfile::tempdir().unwrap();
    let keep = std::env::var_os("DBENTO_SHOTS_DIR").map(std::path::PathBuf::from);
    for theme in ["dark", "light"] {
        let ws = sqlite_workspace();
        exec(
            ws.path(),
            "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT); \
             INSERT INTO t VALUES (1, 'a'), (2, 'b');",
        )
        .await;
        let pilot = Pilot::start().await;
        let mut args = launch_args("headless", ws.path());
        args["env"] = json!({ dbento_pilot::THEME_VAR: theme });
        pilot.ok("launch", args).await;
        click(&pilot, "connection-toggle-lite").await;
        until(&pilot, "the table row", |t| {
            name(t, "tree-row-2") == Some("lite.main.t")
        })
        .await;
        pilot
            .ok(
                "click",
                json!({ "target": { "id": ["tree-row-2"] }, "count": 2 }),
            )
            .await;
        until(&pilot, "two rows", |t| {
            name(t, "footer-rows") == Some("2 rows")
        })
        .await;
        edit(&pilot, 0, "z").await;
        exec(ws.path(), "DELETE FROM t WHERE id = 1").await;
        click(&pilot, "save-edits").await;
        let tree = until(&pilot, "the conflict actions", |t| {
            find_node(t, "edit-banner-drop").is_some()
                && find_node(t, "edit-banner-reload").is_some()
        })
        .await;
        // Park the pointer on the grid so no action is drawn hovered.
        pilot
            .ok("hover", json!({ "target": { "id": ["cell-1-0"] } }))
            .await;
        pilot.ok("wait", json!({ "ms": 300 })).await;
        let shot = shots.path().join(format!("{theme}.png"));
        pilot.ok("screenshot", json!({ "save_to": &shot })).await;
        if let Some(dir) = &keep {
            std::fs::create_dir_all(dir).unwrap();
            pilot
                .ok(
                    "screenshot",
                    json!({ "target": { "id": ["edit-banner"] }, "save_to": dir.join(format!("bannerbtn-{theme}.png")) }),
                )
                .await;
        }
        for id in ["edit-banner-drop", "edit-banner-reload"] {
            let b = common::bounds(&tree, id);
            let mid = b.y + b.h / 2.0;
            let inside = pixel(&shot, b.x.ceil(), mid);
            let beside = pixel(&shot, b.x - 3.0, mid);
            let diff: u32 = inside
                .iter()
                .zip(beside)
                .map(|(a, b)| a.abs_diff(b) as u32)
                .sum();
            assert!(
                diff >= 12,
                "{theme}: {id} has no fill or border ({inside:?} vs {beside:?})"
            );
        }
        assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    }
}
