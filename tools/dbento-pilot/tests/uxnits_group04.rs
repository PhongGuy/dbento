//! uxnits group 04 under gpui-pilot: the explorer's error row, tagged and untagged names
//! aligned, the welcome list keeping a long path's folder name, the history row's time gap
//! and literal-aware label, and the explorer menu's reload glyph.
//! Set DBENTO_SHOTS_DIR to keep the shots.
mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

use common::{Pilot, Rect, bounds, find_node};
use dbento_app::app::{AppPaths, SavedState};
use dbento_app::persist::save_json;
use dbento_app::theme::Appearance;
use dbento_core::{History, HistoryEntry, HistoryOutcome};
use serde_json::{Value, json};

const TOML: &str = "dbento = 1\n\n\
    [connections.lite]\nengine = \"sqlite\"\npath = \"app.db\"\ncolor = \"red\"\n\n\
    [connections.plain]\nengine = \"sqlite\"\npath = \"app.db\"\n\n\
    [connections.zbad]\nengine = \"sqlite\"\npath = \"no/such/folder/app.db\"\n";

const LITERAL_SQL: &str =
    "SELECT 'line one\n-- kept inside the string\nline three'\n-- a note\nFROM t";

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

async fn shot(pilot: &Pilot, file: &Path) {
    pilot.ok("screenshot", json!({ "save_to": file })).await;
}

fn name<'a>(tree: &'a Value, id: &str) -> Option<&'a str> {
    find_node(tree, id).and_then(|n| n["name"].as_str())
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

/// The first column at or right of `from` within `r`'s height whose pixel `hit` accepts.
fn first_column(png_path: &Path, r: Rect, from: f64, hit: impl Fn([i32; 3]) -> bool) -> f64 {
    let px = pixel_reader(png_path);
    let (y0, y1) = (r.y as usize + 2, (r.y + r.h) as usize - 2);
    (from as usize..(r.x + r.w) as usize)
        .find(|&x| (y0..y1).any(|y| hit(px(x, y))))
        .unwrap_or_else(|| panic!("nothing in {r:?} right of {from}")) as f64
}

// Near-white text on the dark theme; tag dots and muted glyphs stay below it.
fn bright(p: [i32; 3]) -> bool {
    p.iter().all(|&c| c > 170)
}

fn workspace() -> tempfile::TempDir {
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("dbento.toml"), TOML).unwrap();
    ws
}

// A state folder in `theme` listing `recent` on the welcome view, with a history of a long
// failure five minutes ago and a statement whose string holds a `--` line.
fn state(theme: Appearance, recent: Vec<PathBuf>, ws: &Path) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let paths = AppPaths::in_dir(dir.path());
    let saved = SavedState {
        appearance: theme,
        recent_workspaces: recent,
        ..SavedState::default()
    };
    save_json(&paths.state, &saved).unwrap();
    let ws = dbento_core::Workspace::open(ws).unwrap();
    let mut h = History::open(&paths.history).unwrap();
    let at = SystemTime::now() - Duration::from_secs(300);
    let failed = HistoryOutcome::Failed {
        message: "no such table: a_table_with_a_rather_long_name_that_does_not_exist_anywhere"
            .into(),
    };
    for (sql, outcome) in [
        ("SELECT * FROM a_table_with_a_rather_long_name", failed),
        (
            LITERAL_SQL,
            HistoryOutcome::Ok {
                rows: 1,
                truncated: false,
                rows_affected: None,
            },
        ),
    ] {
        let e = HistoryEntry::new(at, ws.dir(), "lite", sql, Duration::from_millis(3), outcome);
        h.append(&e).unwrap();
    }
    dir
}

#[tokio::test(flavor = "multi_thread")]
async fn explorer_welcome_and_history_nits() {
    let temp = tempfile::tempdir().unwrap();
    let dir: PathBuf = std::env::var_os("DBENTO_SHOTS_DIR").map_or(temp.path().into(), Into::into);
    std::fs::create_dir_all(&dir).unwrap();
    for (theme, appearance) in [("dark", Appearance::Dark), ("light", Appearance::Light)] {
        let ws = workspace();
        let st = state(appearance, vec![], ws.path());
        let pilot = Pilot::start().await;
        let mut args = common::launch_args("headless", ws.path());
        args["env"] = json!({ "DBENTO_STATE_DIR": st.path() });
        pilot.ok("launch", args).await;

        // A connection that cannot open shows its error row under it.
        pilot
            .ok(
                "click",
                json!({ "target": { "id": ["connection-toggle-zbad"] } }),
            )
            .await;
        let tree = until(&pilot, "the error row", |t| {
            find_node(t, "tree-retry-0").is_some()
        })
        .await;
        pilot.ok("wait", json!({ "ms": 300 })).await;
        let explorer = dir.join(format!("1-2-explorer-{theme}.png"));
        shot(&pilot, &explorer).await;
        eprintln!(
            "{theme}: error row {:?}, sidebar {:?}",
            name(&tree, "tree-row-0"),
            bounds(&tree, "connection-list")
        );

        // The full error is one hover away.
        pilot
            .ok("hover", json!({ "target": { "id": ["tree-row-0"] } }))
            .await;
        pilot.ok("wait", json!({ "ms": 1500 })).await;
        shot(&pilot, &dir.join(format!("1-error-tooltip-{theme}.png"))).await;

        // Tagged and untagged connection names start at the same x.
        if theme == "dark" {
            let (lite, plain) = (
                bounds(&tree, "connection-lite"),
                bounds(&tree, "connection-plain"),
            );
            let from = lite.x + 40.0;
            let (a, b) = (
                first_column(&explorer, lite, from, bright),
                first_column(&explorer, plain, from, bright),
            );
            eprintln!("name starts: tagged {a}, untagged {b}");
            assert!((a - b).abs() <= 1.0, "tagged {a} vs untagged {b}");
        }

        // The explorer menu's reload entry.
        pilot
            .ok(
                "click",
                json!({ "target": { "id": ["explorer-body"] }, "button": "right" }),
            )
            .await;
        until(&pilot, "the explorer menu", |t| {
            find_node(t, "menu-reload").is_some()
        })
        .await;
        pilot.ok("wait", json!({ "ms": 300 })).await;
        shot(&pilot, &dir.join(format!("6-reload-menu-{theme}.png"))).await;
        pilot.ok("key", json!({ "keys": "escape" })).await;

        // History: the time keeps clear of a truncated failure, and the label keeps the
        // string's `--` line.
        pilot
            .ok("click", json!({ "target": { "id": ["history-toggle"] } }))
            .await;
        let tree = until(&pilot, "two history rows", |t| {
            find_node(t, "history-row-1").is_some()
        })
        .await;
        pilot.ok("wait", json!({ "ms": 300 })).await;
        let history = dir.join(format!("4-5-history-{theme}.png"));
        shot(&pilot, &history).await;
        assert_eq!(
            name(&tree, "history-row-0"),
            Some("SELECT 'line one -- kept inside the string line three' FROM t")
        );
        if theme == "dark" {
            let detail = bounds(&tree, "history-detail-1");
            let row = bounds(&tree, "history-row-1");
            let line = Rect {
                y: detail.y,
                h: detail.h,
                ..row
            };
            let right = detail.x + detail.w;
            let time = first_column(&history, line, right, |p| p.iter().all(|&c| c > 90));
            eprintln!("history: detail ends {right}, time starts {time}");
            assert!(
                time - right >= 12.0,
                "only {} px before the time",
                time - right
            );
        }
        assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));

        // The welcome view keeps a long recent path's folder name.
        let long = ws
            .path()
            .join("clients/acme-corporation/projects/2026/quarterly-reporting/warehouse")
            .join("final-folder-name");
        std::fs::create_dir_all(&long).unwrap();
        let st = state(appearance, vec![long], ws.path());
        let pilot = Pilot::start().await;
        let args = json!({ "mode": "headless", "build": false, "width": 1280, "height": 800,
            "scale": 1.0, "args": [], "env": { "DBENTO_STATE_DIR": st.path() } });
        pilot.ok("launch", args).await;
        until(&pilot, "the recent list", |t| {
            find_node(t, "recent-workspace-0").is_some()
        })
        .await;
        pilot.ok("wait", json!({ "ms": 300 })).await;
        shot(&pilot, &dir.join(format!("3-welcome-{theme}.png"))).await;
        assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    }
}
