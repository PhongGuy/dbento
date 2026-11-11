//! uxnits group 05 under gpui-pilot, light theme: the amber footer note reads at 4.5:1 on
//! the footer, both SQL wells show a 3:1 edge on their dialog, and the connection dialog's
//! secondary buttons are shot for the record. Set DBENTO_SHOTS_DIR to keep the shots.
mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::{Pilot, Rect, bounds, find_node, launch_args, sqlite_workspace};
use dbento_app::app::AppPaths;
use dbento_core::{ApprovalRequest, ApprovalStore, Writes};
use serde_json::{Value, json};

const TABLE: &str = "CREATE TABLE t(id INTEGER PRIMARY KEY, name TEXT); \
    INSERT INTO t VALUES (1, 'a'), (2, 'b'), (3, 'c');";
const MANY: &str = "WITH RECURSIVE c(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM c \
    WHERE n < 1500) SELECT n FROM c";
// The light theme's warning token, which the footer note draws in.
const AMBER: [i32; 3] = [0x8F, 0x5F, 0x00];

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

async fn click(pilot: &Pilot, id: &str, count: u32) {
    pilot
        .ok("click", json!({ "target": { "id": [id] }, "count": count }))
        .await;
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

async fn launch_light(pilot: &Pilot, ws: &Path, state: Option<&Path>) {
    let mut args = launch_args("headless", ws);
    args["env"] = match state {
        Some(s) => json!({ "DBENTO_STATE_DIR": s, dbento_pilot::THEME_VAR: "light" }),
        None => json!({ dbento_pilot::THEME_VAR: "light" }),
    };
    pilot.ok("launch", args).await;
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

/// The WCAG contrast ratio of two sRGB pixels.
fn ratio(a: [i32; 3], b: [i32; 3]) -> f64 {
    let lum = |p: [i32; 3]| {
        let ch = |c: i32| {
            let c = f64::from(c) / 255.0;
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * ch(p[0]) + 0.7152 * ch(p[1]) + 0.0722 * ch(p[2])
    };
    let (x, y) = (lum(a), lum(b));
    (x.max(y) + 0.05) / (x.min(y) + 0.05)
}

/// The contrast of `r`'s left edge against the dialog just outside it: the strongest of the
/// columns around the edge, at mid height, against the pixel four columns further out.
fn edge_ratio(png_path: &Path, r: Rect) -> f64 {
    let px = pixel_reader(png_path);
    let y = (r.y + r.h / 2.0) as usize;
    let outside = px(r.x as usize - 4, y);
    (r.x as usize - 1..r.x as usize + 2)
        .map(|x| ratio(px(x, y), outside))
        .fold(0.0, f64::max)
}

#[tokio::test(flavor = "multi_thread")]
async fn amber_note_sql_wells_and_secondary_buttons_in_light() {
    let temp = tempfile::tempdir().unwrap();
    let dir: PathBuf = std::env::var_os("DBENTO_SHOTS_DIR").map_or(temp.path().into(), Into::into);
    std::fs::create_dir_all(&dir).unwrap();

    // A read parked at the 1,000-row cap and then stopped ends truncated: the amber note.
    let ws = sqlite_workspace();
    exec(ws.path(), TABLE).await;
    let pilot = Pilot::start().await;
    launch_light(&pilot, ws.path(), None).await;
    click(&pilot, "connection-lite", 2).await;
    pilot
        .ok("tree", json!({ "wait_for": { "id": ["editor"] } }))
        .await;
    pilot
        .ok(
            "type",
            json!({ "target": { "id": ["editor"] }, "text": MANY }),
        )
        .await;
    click(&pilot, "run-all", 1).await;
    until(&pilot, "the parked cap", |t| {
        name(t, "footer-rows") == Some("1,000+ rows")
    })
    .await;
    key(&pilot, "ctrl-.").await;
    let tree = until(&pilot, "the truncated note", |t| {
        name(t, "footer-note").is_some_and(|n| n.starts_with("truncated at"))
    })
    .await;
    pilot.ok("wait", json!({ "ms": 300 })).await;
    let note_shot = dir.join("amber-note-light.png");
    shot(&pilot, &note_shot).await;
    let note = bounds(&tree, "footer-note");
    let px = pixel_reader(&note_shot);
    let bg = px(note.x as usize - 4, (note.y + note.h / 2.0) as usize);
    let token = ratio(AMBER, bg);
    eprintln!("amber token on footer {bg:?}: {token:.2}");
    assert!(
        token >= 4.5,
        "the amber note's token reads {token:.2} on {bg:?}"
    );
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));

    // The commit preview's SQL well.
    let ws = sqlite_workspace();
    exec(ws.path(), TABLE).await;
    let pilot = Pilot::start().await;
    launch_light(&pilot, ws.path(), None).await;
    click(&pilot, "connection-toggle-lite", 1).await;
    until(&pilot, "the table row", |t| {
        name(t, "tree-row-2") == Some("lite.main.t")
    })
    .await;
    click(&pilot, "tree-row-2", 2).await;
    until(&pilot, "three rows", |t| {
        name(t, "footer-rows") == Some("3 rows")
    })
    .await;
    click(&pilot, "cell-0-1", 1).await;
    key(&pilot, "enter").await;
    until(&pilot, "the cell editor", |t| {
        find_node(t, "cell-editor").is_some()
    })
    .await;
    pilot.ok("type", json!({ "text": "z" })).await;
    key(&pilot, "enter").await;
    click(&pilot, "save-edits", 1).await;
    let tree = until(&pilot, "the prompt", |t| {
        find_node(t, "confirm-sql").is_some()
    })
    .await;
    pilot.ok("wait", json!({ "ms": 500 })).await;
    let preview = dir.join("sql-well-commit-light.png");
    shot(&pilot, &preview).await;
    let edge = edge_ratio(&preview, bounds(&tree, "confirm-sql"));
    eprintln!("commit preview well edge {edge:.2}");
    assert!(
        edge >= 3.0,
        "the commit preview well's edge reads {edge:.2}"
    );
    click(&pilot, "apply-rollback", 1).await;
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));

    // The approval dialog's SQL well, beside its secondary Deny.
    let ws = sqlite_workspace();
    let state = tempfile::tempdir().unwrap();
    let store = ApprovalStore::open(&AppPaths::in_dir(state.path()).approvals).unwrap();
    store
        .request(&ApprovalRequest::new(
            ws.path(),
            "lite",
            "UPDATE t SET name = 'z' WHERE id = 1",
            Writes::Yes,
            vec![],
        ))
        .unwrap();
    let pilot = Pilot::start().await;
    launch_light(&pilot, ws.path(), Some(state.path())).await;
    let tree = until(&pilot, "the approval dialog", |t| {
        find_node(t, "approval-sql").is_some()
    })
    .await;
    pilot.ok("wait", json!({ "ms": 500 })).await;
    let approval = dir.join("sql-well-approval-light.png");
    shot(&pilot, &approval).await;
    let edge = edge_ratio(&approval, bounds(&tree, "approval-sql"));
    eprintln!("approval well edge {edge:.2}");
    assert!(edge >= 3.0, "the approval well's edge reads {edge:.2}");
    click(&pilot, "approval-deny", 1).await;
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));

    // The connection dialog's Test and Cancel, for the secondary-button record.
    let ws = sqlite_workspace();
    let pilot = Pilot::start().await;
    launch_light(&pilot, ws.path(), None).await;
    click(&pilot, "connection-new", 1).await;
    until(&pilot, "the dialog", |t| {
        find_node(t, "conn-save").is_some()
    })
    .await;
    pilot.ok("wait", json!({ "ms": 300 })).await;
    shot(&pilot, &dir.join("secondary-buttons-light.png")).await;
    click(&pilot, "conn-cancel", 1).await;
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
}
