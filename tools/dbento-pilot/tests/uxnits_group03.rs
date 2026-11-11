//! uxnits group 03 under gpui-pilot: the editor menu greys Cut and Copy without a selection,
//! no row tooltip draws over the explorer menu, the approval path tooltip sits beside its
//! line clear of the SQL, and the commit preview sets its SQL 9 px inside the box.
//! Set DBENTO_SHOTS_DIR to keep the shots.
mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::{Pilot, Rect, bounds, find_node, launch_args, sqlite_workspace};
use dbento_app::app::AppPaths;
use dbento_core::{ApprovalRequest, ApprovalStore, Writes};
use serde_json::{Value, json};

const TABLE: &str = "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT); \
    INSERT INTO t VALUES (1, 'a'), (2, 'b'), (3, 'c');";

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

async fn right_click(pilot: &Pilot, id: &str) {
    pilot
        .ok(
            "click",
            json!({ "target": { "id": [id] }, "button": "right" }),
        )
        .await;
}

async fn hover(pilot: &Pilot, id: &str, ms: u64) {
    pilot.ok("hover", json!({ "target": { "id": [id] } })).await;
    pilot.ok("wait", json!({ "ms": ms })).await;
}

async fn key(pilot: &Pilot, keys: &str) {
    pilot.ok("key", json!({ "keys": keys })).await;
}

fn name<'a>(tree: &'a Value, id: &str) -> Option<&'a str> {
    find_node(tree, id).and_then(|n| n["name"].as_str())
}

async fn shot(pilot: &Pilot, file: &Path) {
    pilot.ok("screenshot", json!({ "save_to": file })).await;
}

/// The RGB pixel at `x`, `y` of a whole-window PNG shot at scale 1.
fn pixel_reader(png_path: &Path) -> impl Fn(usize, usize) -> [i32; 3] {
    let bytes = std::fs::read(png_path).unwrap();
    let mut reader = png::Decoder::new(std::io::Cursor::new(bytes))
        .read_info()
        .unwrap();
    let mut buf = vec![0; reader.output_buffer_size().unwrap()];
    let frame = reader.next_frame(&mut buf).unwrap();
    let (line, step) = (frame.line_size, frame.line_size / frame.width as usize);
    move |x, y| {
        let at = y * line + x * step;
        [buf[at] as i32, buf[at + 1] as i32, buf[at + 2] as i32]
    }
}

/// How far right of `r`'s left edge the first text pixel sits: the first column, inside the
/// border, holding a pixel far from the fill along `r`'s first text line.
fn text_inset(png_path: &Path, r: Rect) -> f64 {
    let px = pixel_reader(png_path);
    let (x0, y0) = (r.x as usize + 2, r.y as usize + 4);
    let fill = px(x0, y0);
    let far = |x: usize, y: usize| {
        let p = px(x, y);
        (0..3).map(|i| (p[i] - fill[i]).abs()).sum::<i32>() > 120
    };
    let left = (x0..x0 + 60)
        .find(|&x| (y0..y0 + 16).any(|y| far(x, y)))
        .unwrap_or_else(|| panic!("no text in {r:?} of {}", png_path.display()));
    left as f64 - r.x
}

/// Whether some pixel of `r` differs from what `before` drew there: a tooltip covering it.
fn changed(before: &Path, after: &Path, r: Rect) -> bool {
    let (a, b) = (pixel_reader(before), pixel_reader(after));
    let (x0, y0) = (r.x as usize + 2, r.y as usize + 2);
    let (x1, y1) = ((r.x + r.w) as usize - 2, (r.y + r.h) as usize - 2);
    (y0..y1).any(|y| (x0..x1).any(|x| a(x, y) != b(x, y)))
}

#[tokio::test(flavor = "multi_thread")]
async fn editor_menu_explorer_tip_approval_tip_and_preview_inset() {
    let temp = tempfile::tempdir().unwrap();
    let dir: PathBuf = std::env::var_os("DBENTO_SHOTS_DIR").map_or(temp.path().into(), Into::into);
    std::fs::create_dir_all(&dir).unwrap();
    for theme in ["dark", "light"] {
        let ws = sqlite_workspace();
        exec(ws.path(), TABLE).await;
        let pilot = Pilot::start().await;
        let mut args = launch_args("headless", ws.path());
        args["env"] = json!({ dbento_pilot::THEME_VAR: theme });
        pilot.ok("launch", args).await;

        // A resting pointer shows the connection's tooltip; the menu opened on it hides it.
        hover(&pilot, "connection-lite", 1500).await;
        shot(&pilot, &dir.join(format!("tip-before-menu-{theme}.png"))).await;
        right_click(&pilot, "connection-lite").await;
        until(&pilot, "the explorer menu", |t| {
            find_node(t, "menu-new-query").is_some()
        })
        .await;
        pilot.ok("wait", json!({ "ms": 1500 })).await;
        shot(&pilot, &dir.join(format!("tip-menu-{theme}.png"))).await;
        key(&pilot, "escape").await;

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

        // A right-click below the browse SQL leaves no selection: Cut and Copy are greyed.
        right_click(&pilot, "editor").await;
        until(&pilot, "the editor menu", |t| {
            find_node(t, "editor-menu-cut").is_some()
        })
        .await;
        shot(&pilot, &dir.join(format!("editor-menu-{theme}.png"))).await;
        key(&pilot, "escape").await;

        // The commit preview's SQL starts 9 px inside its box, as the approval dialog's does.
        click(&pilot, "cell-0-1").await;
        key(&pilot, "enter").await;
        until(&pilot, "the cell editor", |t| {
            find_node(t, "cell-editor").is_some()
        })
        .await;
        pilot.ok("type", json!({ "text": "z" })).await;
        key(&pilot, "enter").await;
        until(&pilot, "one pending change", |t| {
            name(t, "pending-changes") == Some("1 pending change")
        })
        .await;
        click(&pilot, "save-edits").await;
        let tree = until(&pilot, "the prompt", |t| {
            find_node(t, "confirm-sql").is_some()
        })
        .await;
        pilot.ok("wait", json!({ "ms": 500 })).await;
        let preview = dir.join(format!("commit-preview-{theme}.png"));
        shot(&pilot, &preview).await;
        let inset = text_inset(&preview, bounds(&tree, "confirm-sql"));
        eprintln!("{theme}: preview text inset {inset}");
        assert!(
            (8.0..=11.0).contains(&inset),
            "the preview SQL starts {inset} px into its box"
        );
        click(&pilot, "apply-rollback").await;
        assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));

        // The approval dialog's path tooltip opens beside the folder name, clear of the SQL.
        let ws = sqlite_workspace();
        let state = tempfile::tempdir().unwrap();
        let store = ApprovalStore::open(&AppPaths::in_dir(state.path()).approvals).unwrap();
        let sql = "UPDATE t SET name = 'z' WHERE id = 1";
        store
            .request(&ApprovalRequest::new(
                ws.path(),
                "lite",
                sql,
                Writes::Yes,
                vec![],
            ))
            .unwrap();
        let pilot = Pilot::start().await;
        let mut args = launch_args("headless", ws.path());
        args["env"] = json!({ "DBENTO_STATE_DIR": state.path(), dbento_pilot::THEME_VAR: theme });
        pilot.ok("launch", args).await;
        let tree = until(&pilot, "the approval dialog", |t| {
            find_node(t, "approval-sql").is_some()
        })
        .await;
        pilot.ok("wait", json!({ "ms": 500 })).await;
        let plain = dir.join(format!("approval-plain-{theme}.png"));
        shot(&pilot, &plain).await;
        hover(&pilot, "approval-workspace", 1500).await;
        let tipped = dir.join(format!("approval-path-tooltip-{theme}.png"));
        shot(&pilot, &tipped).await;
        let (headline, line, sql_box) = (
            bounds(&tree, "approval-connection"),
            bounds(&tree, "approval-workspace"),
            bounds(&tree, "approval-sql"),
        );
        // The lines sit a gap_2 apart and the SQL box adds its mt_1: half a gap more.
        let gap = line.y - headline.y - headline.h;
        assert_eq!(
            (line.h, sql_box.y - line.y - line.h),
            (headline.h, gap * 1.5),
            "the folder line keeps the headline's height and the gap above the SQL box"
        );
        let beside = Rect {
            x: line.x + line.w,
            w: 200.0,
            ..line
        };
        assert!(
            changed(&plain, &tipped, beside),
            "no tooltip beside the line"
        );
        // The tooltip's soft shadow may reach the box's top few pixels; its text may not.
        let sql_text = Rect {
            y: sql_box.y + 6.0,
            h: sql_box.h - 8.0,
            ..sql_box
        };
        for (what, r) in [("SQL", sql_text), ("headline", headline)] {
            assert!(
                !changed(&plain, &tipped, r),
                "the tooltip covers the {what}"
            );
        }
        assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    }
}
