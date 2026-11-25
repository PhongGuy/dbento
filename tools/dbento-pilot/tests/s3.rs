//! S3 under gpui-pilot against Moto: dialog, tree, object tab, download and `ls` listing.
mod common;
use std::path::Path;
use std::time::{Duration, Instant};

use common::{Pilot, bounds, find_node, launch_args};
use dbento_testkit::{S3_ACCESS_KEY, S3_REGION, S3_SECRET, S3_SECRET_VAR, S3Moto, s3_moto};
use serde_json::{Value, json};

// Helpers copied from tests/dynamo.rs; `launch` passes the Moto secret as S3_SECRET_VAR.
async fn until(pilot: &Pilot, what: &str, done: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let tree = pilot.ok("tree", json!({})).await;
        if done(&tree) {
            return tree;
        }
        assert!(Instant::now() < deadline, "{what} never happened: {tree}");
        pilot.ok("wait", json!({ "ms": 50 })).await;
    }
}
fn name<'a>(tree: &'a Value, id: &str) -> Option<&'a str> {
    find_node(tree, id).and_then(|n| n["name"].as_str())
}
/// Each rendered tree row as its index and label. Row ids carry the absolute row index, which
/// passes 500 once a folder of 1,200 keys is open, so every node is scanned rather than a range.
fn rows(tree: &Value) -> impl Iterator<Item = (usize, Option<&str>)> {
    tree["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|n| {
            let id = n["id"]
                .as_str()
                .or_else(|| n["path"].as_array()?.last()?.as_str())?;
            let ix = id.strip_prefix("tree-row-")?.parse().ok()?;
            Some((ix, n["name"].as_str()))
        })
}
fn row_named(tree: &Value, aria: &str) -> Option<usize> {
    rows(tree).find_map(|(ix, label)| (label == Some(aria)).then_some(ix))
}
/// The lowest row index the explorer renders, which moves only while a scroll moves the list.
fn first_row(tree: &Value) -> Option<usize> {
    rows(tree).map(|(ix, _)| ix).min()
}
/// Scrolls the explorer until the row labelled `aria` lies wholly inside the list. An unseen
/// row is sought downward first, then upward once a scroll stops moving the list.
async fn seek(pilot: &Pilot, aria: &str) -> (Value, usize) {
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut sweep = 2;
    let mut last = None;
    loop {
        let tree = pilot.ok("tree", json!({})).await;
        assert!(Instant::now() < deadline, "row {aria} never showed: {tree}");
        if find_node(&tree, "schema-tree").is_none() {
            pilot.ok("wait", json!({ "ms": 50 })).await;
            continue;
        }
        let list = bounds(&tree, "schema-tree");
        let notches = match row_named(&tree, aria) {
            None => {
                let first = first_row(&tree);
                if last.is_some() && first == last {
                    sweep = -sweep;
                }
                last = first;
                sweep
            }
            Some(ix) => {
                let row = bounds(&tree, &format!("tree-row-{ix}"));
                if row.y < list.y - 0.5 {
                    -1
                } else if row.y + row.h > list.y + list.h + 0.5 {
                    1
                } else {
                    return (tree, ix);
                }
            }
        };
        let at = json!({ "x": list.x + list.w / 2.0, "y": list.y + list.h / 2.0 });
        pilot
            .ok(
                "scroll",
                json!({ "target": at, "notches": notches, "notch_ms": 1, "settle_ms": 50 }),
            )
            .await;
    }
}
async fn click(pilot: &Pilot, id: &str, extra: Value) {
    let mut args = json!({ "target": { "id": [id] } });
    if let (Some(a), Some(e)) = (args.as_object_mut(), extra.as_object()) {
        a.extend(e.clone());
    }
    pilot.ok("click", args).await;
}
/// Picks the segmented option labelled `label`.
async fn option(pilot: &Pilot, label: &str) {
    pilot
        .ok(
            "click",
            json!({ "target": { "role": "Tab", "name": label } }),
        )
        .await;
}
async fn fill(pilot: &Pilot, id: &str, text: &str) {
    pilot
        .ok("type", json!({ "target": { "id": [id] }, "text": text }))
        .await;
}

/// Clicks `part` (`tree-row`, `tree-toggle`) of the row named `aria`, with `extra`.
async fn row(pilot: &Pilot, aria: &str, part: &str, extra: Value) {
    let (_, ix) = seek(pilot, aria).await;
    click(pilot, &format!("{part}-{ix}"), extra).await;
}

async fn launch(ws: &Path) -> Pilot {
    let pilot = Pilot::start().await;
    let mut args = launch_args("headless", ws);
    args["env"] = json!({ S3_SECRET_VAR: S3_SECRET });
    pilot.ok("launch", args).await;
    pilot
}

/// A workspace whose one connection, `s3`, is the handle's bucket.
fn workspace(s3: &S3Moto) -> tempfile::TempDir {
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(
        ws.path().join("dbento.toml"),
        format!("dbento = 1\n{}", s3.connection_toml("s3", "")),
    )
    .unwrap();
    ws
}

/// Opens the `s3` connection row, waits for its lone bucket to open itself, then opens the
/// prefixes in `path`. A toggle on the bucket would close it again.
async fn open_bucket(pilot: &Pilot, b: &str, path: &[&str]) {
    click(pilot, "connection-toggle-s3", json!({})).await;
    let inside = [format!("s3.{b}/"), format!("s3.{b}.")];
    until(pilot, "the bucket's first level", |t| {
        rows(t).any(|(_, l)| l.is_some_and(|l| inside.iter().any(|p| l.starts_with(p.as_str()))))
    })
    .await;
    for p in path {
        row(pilot, &format!("s3.{b}/{p}"), "tree-toggle", json!({})).await;
    }
}

/// Wheels the dialog body to its end, so the fields under its fold come into view.
async fn body_down(pilot: &Pilot) {
    let tree = pilot.ok("tree", json!({})).await;
    let body = bounds(&tree, "conn-body");
    pilot
        .ok(
            "scroll",
            json!({ "target": { "x": body.x + body.w / 2.0, "y": body.y + body.h / 2.0 }, "notches": 20, "settle_ms": 300 }),
        )
        .await;
}

/// Saves a screenshot of the window at `path` for a look after the run.
async fn shot(pilot: &Pilot, path: &Path) {
    pilot.ok("screenshot", json!({ "save_to": path })).await;
}

/// Where a test leaves its screenshots, kept after the run for a look.
fn shots() -> std::path::PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR")).join("s3-shots");
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

const ENGINE_LABELS: [&str; 10] = [
    "Postgres",
    "SQLite",
    "MySQL",
    "DuckDB",
    "SQL Server",
    "SurrealDB",
    "MongoDB",
    "Redis",
    "DynamoDB",
    "S3",
];

/// Asserts all ten engine cards are drawn, each wholly inside the engine grid and the dialog.
fn ten_cards_fit(tree: &Value) {
    let (grid, dialog) = (bounds(tree, "conn-engine"), bounds(tree, "conn-dialog"));
    for label in ENGINE_LABELS {
        let card = tree["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|n| n["role"] == json!("Tab") && n["name"] == json!(label))
            .unwrap_or_else(|| panic!("no {label} card: {tree}"));
        let b = &card["bounds"];
        let (x, y, w, h) = (
            b["x"].as_f64().unwrap(),
            b["y"].as_f64().unwrap(),
            b["w"].as_f64().unwrap(),
            b["h"].as_f64().unwrap(),
        );
        for outer in [grid, dialog] {
            assert!(
                x >= outer.x - 0.5
                    && y >= outer.y - 0.5
                    && x + w <= outer.x + outer.w + 0.5
                    && y + h <= outer.y + outer.h + 0.5,
                "{label} card {b} is not inside {outer:?}"
            );
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn an_s3_connection_made_in_the_dialog_saves_its_buckets_and_lists_them() {
    let s3 = s3_moto().await;
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("dbento.toml"), "dbento = 1\n").unwrap();
    let pilot = launch(ws.path()).await;
    click(&pilot, "connection-new", json!({})).await;
    until(&pilot, "the dialog", |t| {
        find_node(t, "conn-name").is_some()
    })
    .await;
    option(&pilot, "S3").await;
    let tree = until(&pilot, "the S3 form", |t| {
        find_node(t, "conn-buckets").is_some()
    })
    .await;
    ten_cards_fit(&tree);
    shot(&pilot, &shots().join("s3-dialog.png")).await;
    fill(&pilot, "conn-name", "s3").await;
    fill(&pilot, "conn-region", S3_REGION).await;
    fill(&pilot, "conn-endpoint", s3.endpoint()).await;
    fill(&pilot, "conn-access-key-id", S3_ACCESS_KEY).await;
    assert!(
        find_node(&pilot.ok("tree", json!({})).await, "conn-virtual-hosted").is_some(),
        "the S3 form offers virtual-hosted addressing"
    );
    // The S3 form runs past the body's fold; wheel it down before each lower field.
    body_down(&pilot).await;
    option(&pilot, "Environment variable").await;
    fill(&pilot, "conn-env", S3_SECRET_VAR).await;
    body_down(&pilot).await;
    fill(&pilot, "conn-buckets", s3.bucket()).await;
    shot(&pilot, &shots().join("s3-dialog-filled.png")).await;
    click(&pilot, "conn-test", json!({})).await;
    let tree = until(&pilot, "the test", |t| {
        name(t, "conn-test-result").is_some_and(|r| r != "Testing\u{2026}")
    })
    .await;
    assert_eq!(name(&tree, "conn-test-result"), Some("Connected"));
    click(&pilot, "conn-save", json!({})).await;
    until(&pilot, "the dialog to close", |t| {
        find_node(t, "conn-dialog").is_none()
    })
    .await;
    let toml = std::fs::read_to_string(ws.path().join("dbento.toml")).unwrap();
    for want in [
        "engine = \"s3\"",
        &format!("buckets = [\"{}\"]", s3.bucket()),
        S3_SECRET_VAR,
    ] {
        assert!(toml.contains(want), "{want}: {toml}");
    }
    assert!(!toml.contains(S3_SECRET), "{toml}");
    open_bucket(&pilot, s3.bucket(), &[]).await;
    until(&pilot, "the root prefixes", |t| {
        row_named(t, &format!("s3.{}/text/", s3.bucket())).is_some()
    })
    .await;
    shot(&pilot, &shots().join("s3-tree.png")).await;
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    pilot.assert_bus_untouched();
}

#[tokio::test(flavor = "multi_thread")]
async fn the_tree_pages_shows_hostile_keys_escaped_and_opens_an_object() {
    let s3 = s3_moto().await;
    s3.seed_paged().await;
    let b = s3.bucket().to_owned();
    let ws = workspace(&s3);
    let pilot = launch(ws.path()).await;
    open_bucket(&pilot, &b, &["many/"]).await;
    let more = format!("s3.{b}/many/:more");
    // The explorer renders only the rows in view, so Load more and the second page's
    // rows are scrolled to rather than waited for.
    row(&pilot, &more, "tree-row", json!({})).await;
    seek(&pilot, &format!("s3.{b}.many/00999")).await;
    row(&pilot, &format!("s3.{b}/many/"), "tree-toggle", json!({})).await;
    row(
        &pilot,
        &format!("s3.{b}/hostile/"),
        "tree-toggle",
        json!({}),
    )
    .await;
    let tree = until(&pilot, "the hostile keys", |t| {
        row_named(t, &format!("s3.{b}.hostile/tab\\u{{9}}name")).is_some()
    })
    .await;
    for (_, label) in rows(&tree) {
        let label = label.unwrap_or_default();
        assert!(
            !label.chars().any(|c| c.is_control() || c == '\u{202e}'),
            "{label:?}"
        );
    }
    shot(&pilot, &shots().join("s3-hostile.png")).await;
    row(&pilot, &format!("s3.{b}/text/"), "tree-toggle", json!({})).await;
    row(
        &pilot,
        &format!("s3.{b}.text/hello.txt"),
        "tree-row",
        json!({ "count": 2 }),
    )
    .await;
    let tree = until(&pilot, "the object tab", |t| {
        find_node(t, "object-header").is_some()
    })
    .await;
    assert!(
        name(&tree, "object-header").is_some_and(|h| h.contains("13 bytes")),
        "{tree}"
    );
    shot(&pilot, &shots().join("s3-object-text.png")).await;
    click(&pilot, "object-mode-hex", json!({})).await;
    until(&pilot, "the hex rows", |t| {
        find_node(t, "object-hex-row-0").is_some()
    })
    .await;
    shot(&pilot, &shots().join("s3-object.png")).await;
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_download_lands_through_the_save_dialog_and_its_cancel_goes_away() {
    let s3 = s3_moto().await;
    let b = s3.bucket().to_owned();
    let ws = workspace(&s3);
    let out = ws.path().join("three.bin");
    let pilot = launch(ws.path()).await;
    open_bucket(&pilot, &b, &["big/"]).await;
    row(
        &pilot,
        &format!("s3.{b}.big/three-mib.bin"),
        "tree-row",
        json!({ "button": "right" }),
    )
    .await;
    until(&pilot, "the object menu", |t| {
        find_node(t, "tree-menu-download").is_some()
    })
    .await;
    click(&pilot, "tree-menu-download", json!({})).await;
    pilot.ok("pick_file", json!({ "paths": [&out] })).await;
    let tree = until(&pilot, "the finished download", |t| {
        find_node(t, "download-dismiss-0").is_some()
    })
    .await;
    assert!(
        find_node(&tree, "download-cancel-0").is_none(),
        "no cancel once it ended"
    );
    shot(&pilot, &shots().join("s3-download.png")).await;
    let got = std::fs::read(&out).unwrap();
    assert_eq!(got.len(), dbento_testkit::THREE_MIB);
    assert!(
        got.iter()
            .enumerate()
            .all(|(i, b)| *b == dbento_testkit::big_byte(i)),
        "the file holds the object's bytes in order"
    );
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_prefix_opens_as_an_ls_listing_with_escaped_cells() {
    let s3 = s3_moto().await;
    let b = s3.bucket().to_owned();
    let ws = workspace(&s3);
    let pilot = launch(ws.path()).await;
    open_bucket(&pilot, &b, &[]).await;
    row(
        &pilot,
        &format!("s3.{b}/hostile/"),
        "tree-row",
        json!({ "button": "right" }),
    )
    .await;
    until(&pilot, "the prefix menu", |t| {
        find_node(t, "tree-menu-open-listing").is_some()
    })
    .await;
    click(&pilot, "tree-menu-open-listing", json!({})).await;
    let tree = until(&pilot, "the listing", |t| {
        find_node(t, "cell-0-1").is_some()
    })
    .await;
    for r in 0..9 {
        if let Some(cell) = name(&tree, &format!("cell-{r}-1")) {
            assert!(
                !cell.chars().any(|c| c.is_control() || c == '\u{202e}'),
                "row {r}: {cell:?}"
            );
        }
    }
    shot(&pilot, &shots().join("s3-listing.png")).await;
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_long_line_and_a_binary_object_preview_without_stalling() {
    let s3 = s3_moto().await;
    s3.put("long/line.txt", vec![b'a'; 1 << 20], Some("text/plain"))
        .await;
    let b = s3.bucket().to_owned();
    let ws = workspace(&s3);
    let pilot = launch(ws.path()).await;
    open_bucket(&pilot, &b, &["long/"]).await;
    row(
        &pilot,
        &format!("s3.{b}.long/line.txt"),
        "tree-row",
        json!({ "count": 2 }),
    )
    .await;
    until(&pilot, "the long text", |t| {
        find_node(t, "object-text").is_some()
    })
    .await;
    let started = Instant::now();
    click(&pilot, "object-mode-hex", json!({})).await;
    until(&pilot, "the hex rows", |t| {
        find_node(t, "object-hex-row-0").is_some()
    })
    .await;
    let took = started.elapsed();
    eprintln!("s3 preview: text to hex took {took:?}");
    assert!(
        took < Duration::from_secs(2),
        "switching a 1 MiB line took {took:?}"
    );
    shot(&pilot, &shots().join("s3-long-hex.png")).await;
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
}
