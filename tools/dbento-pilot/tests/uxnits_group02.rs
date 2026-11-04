//! uxnits group 02 under gpui-pilot: an open record field keeps its text where the read-only
//! value drew it, and the record view, inspector and status bar show the was line, the
//! multi-line hint, a disabled Revert while saving and a conflicting delete's fresh line.
//! Set DBENTO_SHOTS_DIR to keep the shots.
mod common;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use common::{Pilot, Rect, bounds, find_node, launch_args, sqlite_workspace};
use serde_json::{Value, json};

const TABLE: &str = "CREATE TABLE n(id INTEGER PRIMARY KEY, w TEXT, b BLOB); \
    INSERT INTO n VALUES (1, 'one', x'0102030405060708090a'), (2, 'a' || char(10) || 'b', NULL), \
    (3, 'three', NULL);";

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

async fn click(pilot: &Pilot, id: &str) {
    pilot.ok("click", json!({ "target": { "id": [id] } })).await;
}

async fn key(pilot: &Pilot, keys: &str) {
    pilot.ok("key", json!({ "keys": keys })).await;
}

fn name<'a>(tree: &'a Value, id: &str) -> Option<&'a str> {
    find_node(tree, id).and_then(|n| n["name"].as_str())
}

async fn shot(pilot: &Pilot, file: &Path, id: Option<&str>) {
    let target = id.map(|id| json!({ "id": [id] }));
    pilot
        .ok("screenshot", json!({ "target": target, "save_to": file }))
        .await;
}

/// The RGB pixels of a whole-window PNG shot at scale 1, with its row length.
fn pixels(png_path: &Path) -> (Vec<u8>, usize, usize) {
    let bytes = std::fs::read(png_path).unwrap();
    let mut reader = png::Decoder::new(std::io::Cursor::new(bytes))
        .read_info()
        .unwrap();
    let mut buf = vec![0; reader.output_buffer_size().unwrap()];
    let frame = reader.next_frame(&mut buf).unwrap();
    let step = frame.line_size / frame.width as usize;
    (buf, frame.line_size, step)
}

/// The left and top edges of the first glyph in `r`: the first column with some but not most
/// pixels far from the fill at `r`'s top-left (so a border crossing the band is passed over),
/// then the first inked row of that glyph.
fn ink_corner(png_path: &Path, r: Rect) -> (f64, f64) {
    let (buf, line, step) = pixels(png_path);
    let px = |x: usize, y: usize| {
        let at = y * line + x * step;
        [buf[at] as i32, buf[at + 1] as i32, buf[at + 2] as i32]
    };
    let (x0, x1) = (r.x as usize, (r.x + r.w) as usize);
    let (y0, y1) = (r.y as usize, (r.y + r.h) as usize);
    let bg = px(x0, y0);
    let far = |x: usize, y: usize| {
        let p = px(x, y);
        (0..3).map(|i| (p[i] - bg[i]).abs()).sum::<i32>() > 120
    };
    let some = |ink: usize, of: usize| ink > 0 && ink * 4 < of * 3;
    let left = (x0..x1)
        .find(|&x| some((y0..y1).filter(|&y| far(x, y)).count(), y1 - y0))
        .unwrap_or_else(|| panic!("no glyph in {r:?} of {}", png_path.display()));
    // The top is read over the first glyph alone, clear of the editor's caret.
    let top = (y0..y1)
        .find(|&y| (left..left + 6).any(|x| far(x, y)))
        .unwrap_or_else(|| panic!("no glyph top in {r:?} of {}", png_path.display()));
    (left as f64, top as f64)
}

/// The band a value's first line of text sits in, inside where an open editor's border and
/// focus ring draw.
fn band(value: Rect) -> Rect {
    Rect {
        x: value.x - 3.0,
        y: value.y + 1.0,
        w: 120.0,
        h: 16.0,
    }
}

/// Shots `<what>-display-<theme>` and `<what>-editing-<theme>` of field `value` before and
/// after a click opens `editor`, asserting its first glyph stays put.
async fn assert_text_stays(
    pilot: &Pilot,
    dir: &Path,
    what: &str,
    theme: &str,
    value: &str,
    editor: &str,
) {
    let shown = pilot.ok("tree", json!({})).await;
    let r = band(bounds(&shown, value));
    let still = dir.join(format!("{what}-display-{theme}.png"));
    shot(pilot, &still, None).await;
    click(pilot, value).await;
    key(pilot, "end").await;
    until(pilot, "the editor", |t| find_node(t, editor).is_some()).await;
    let editing = dir.join(format!("{what}-editing-{theme}.png"));
    shot(pilot, &editing, None).await;
    let (before, after) = (ink_corner(&still, r), ink_corner(&editing, r));
    eprintln!("{what} {theme}: ink {before:?} -> {after:?}");
    assert!(
        (after.0 - before.0).abs() <= 1.0 && (after.1 - before.1).abs() <= 1.0,
        "{what} text moved from {before:?} to {after:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn record_fields_editing_and_conflicts() {
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
        click(&pilot, "connection-toggle-lite").await;
        until(&pilot, "the table row", |t| {
            name(t, "tree-row-2") == Some("lite.main.n")
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

        // Opening a field keeps its first glyph where the read-only value drew it.
        click(&pilot, "row-view-expand").await;
        until(&pilot, "the record", |t| {
            name(t, "record-value-1") == Some("one")
        })
        .await;
        assert_text_stays(
            &pilot,
            &dir,
            "inset",
            theme,
            "record-value-1",
            "record-editor",
        )
        .await;

        // Shift+Enter makes the value multi-line, so the hint switches and Enter adds a line.
        key(&pilot, "shift-enter").await;
        until(&pilot, "the multi-line hint", |t| {
            name(t, "record-hint").is_some_and(|h| h.contains("Enter new line"))
        })
        .await;
        key(&pilot, "enter").await;
        pilot.ok("type", json!({ "text": "x" })).await;
        shot(
            &pilot,
            &dir.join(format!("hint-multiline-{theme}.png")),
            Some("record-view"),
        )
        .await;
        key(&pilot, "ctrl-enter").await;

        // Capped bytes offer Copy original beside Revert.
        click(&pilot, "record-value-2").await;
        until(&pilot, "the bytes editor", |t| {
            find_node(t, "record-editor").is_some()
        })
        .await;
        key(&pilot, "ctrl-a").await;
        pilot.ok("type", json!({ "text": "\\xff" })).await;
        key(&pilot, "enter").await;
        until(&pilot, "the bytes was line", |t| {
            find_node(t, "record-was-copy-2").is_some()
        })
        .await;
        shot(
            &pilot,
            &dir.join(format!("bytes-copy-{theme}.png")),
            Some("record-view"),
        )
        .await;

        // While the save's prompt waits, Revert is greyed out.
        click(&pilot, "save-edits").await;
        until(&pilot, "the prompt", |t| {
            find_node(t, "apply-rollback").is_some()
        })
        .await;
        shot(
            &pilot,
            &dir.join(format!("revert-saving-{theme}.png")),
            None,
        )
        .await;
        click(&pilot, "apply-rollback").await;
        until(&pilot, "the rollback", |t| {
            find_node(t, "apply-rollback").is_none()
        })
        .await;
        click(&pilot, "revert-edits").await;

        // Back in the grid the cell stays selected, so the status bar keeps its was line,
        // which writes a multi-line original on one line.
        click(&pilot, "row-view-expand").await;
        click(&pilot, "cell-1-1").await;
        click(&pilot, "inspector-toggle").await;
        until(&pilot, "the inspector", |t| {
            find_node(t, "inspector-value-1").is_some()
        })
        .await;
        assert_text_stays(
            &pilot,
            &dir,
            "inspector-inset",
            theme,
            "inspector-value-1",
            "inspector-editor",
        )
        .await;
        key(&pilot, "ctrl-a").await;
        pilot.ok("type", json!({ "text": "z" })).await;
        key(&pilot, "ctrl-enter").await;
        until(&pilot, "the status was line", |t| {
            name(t, "status-was") == Some("Was: \"a\\nb\"")
        })
        .await;
        shot(
            &pilot,
            &dir.join(format!("inspector-cell-{theme}.png")),
            None,
        )
        .await;
        click(&pilot, "inspector-toggle").await;
        click(&pilot, "row-view-expand").await;
        click(&pilot, "row-view-expand").await;
        until(&pilot, "the was line back in the grid", |t| {
            name(t, "status-was") == Some("Was: \"a\\nb\"")
        })
        .await;
        shot(&pilot, &dir.join(format!("grid-return-{theme}.png")), None).await;
        click(&pilot, "revert-edits").await;

        // A delete whose row went elsewhere says so after Reload, in the inspector and the
        // status bar.
        click(&pilot, "cell-0-1").await;
        click(&pilot, "inspector-toggle").await;
        click(&pilot, "inspector-delete").await;
        exec(ws.path(), "DELETE FROM n WHERE id = 1").await;
        click(&pilot, "save-edits").await;
        until(&pilot, "the banner", |t| {
            find_node(t, "edit-banner-reload").is_some()
        })
        .await;
        click(&pilot, "edit-banner-reload").await;
        until(&pilot, "the fresh row line", |t| {
            name(t, "inspector-fresh-row") == Some("now in database: row deleted")
                && name(t, "status-was") == Some("now in database: row deleted")
        })
        .await;
        shot(&pilot, &dir.join(format!("delete-gone-{theme}.png")), None).await;
        assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    }
}
