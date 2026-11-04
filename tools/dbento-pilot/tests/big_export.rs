//! Exporting a million loaded rows under gpui-pilot: the window stays responsive while
//! the export renders, and the file holds every row.
mod common;

use std::time::{Duration, Instant};

use common::{Pilot, find_node, launch_args, rows_loaded, run_query, sqlite_workspace};
use serde_json::{Value, json};

/// A million typed rows without a table, as the wheel-scroll recording uses.
const MILLION: &str = "WITH RECURSIVE r(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM r WHERE n < 1000000) SELECT n, n * 7 % 1000 AS code, printf('row %d', n) AS label, n / 3.0 AS ratio FROM r";
/// The slowest a tree read may take while the export runs. Rendering a million rows on the
/// main thread stalled it for far longer; see the commit that added this test.
const MAX_STALL: Duration = Duration::from_millis(250);

fn name<'a>(tree: &'a Value, id: &str) -> Option<&'a str> {
    find_node(tree, id).and_then(|n| n["name"].as_str())
}

/// Polls the tree until `done` holds, for at most `secs`; returns that tree.
async fn until(pilot: &Pilot, what: &str, secs: u64, done: impl Fn(&Value) -> bool) -> Value {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        let tree = pilot.ok("tree", json!({})).await;
        if done(&tree) {
            return tree;
        }
        assert!(Instant::now() < deadline, "{what} never happened: {tree}");
        pilot.ok("wait", json!({ "ms": 100 })).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_million_row_export_keeps_the_window_responsive() {
    let ws = sqlite_workspace();
    let pilot = Pilot::start().await;
    pilot.ok("launch", launch_args("headless", ws.path())).await;
    // Park at the default cap, then load the rest so the export has every row to render.
    run_query(&pilot, MILLION, false).await;
    until(&pilot, "the cap park", 30, |t| {
        find_node(t, "fetch-all").is_some()
    })
    .await;
    pilot
        .ok("click", json!({ "target": { "id": ["fetch-all"] } }))
        .await;
    until(&pilot, "a million loaded rows", 120, |t| {
        rows_loaded(t) == 1_000_000
    })
    .await;

    // Answer the save dialog, then poll the tree without settling until the notice shows:
    // each read is answered on the main thread, so the slowest one is the longest stall.
    let out = ws.path().join("big.csv");
    pilot
        .ok("click", json!({ "target": { "id": ["result-export"] } }))
        .await;
    pilot
        .ok("pick_file", json!({ "paths": [&out], "settle_ms": 0 }))
        .await;
    let deadline = Instant::now() + Duration::from_secs(60);
    let (mut worst, mut reads) = (Duration::ZERO, 0);
    loop {
        let asked = Instant::now();
        let tree = pilot.ok("tree", json!({ "settle_ms": 0 })).await;
        worst = worst.max(asked.elapsed());
        reads += 1;
        if name(&tree, "notice") == Some("Exported 1,000,000 rows to big.csv") {
            break;
        }
        assert!(Instant::now() < deadline, "no export notice: {tree}");
        pilot.ok("wait", json!({ "ms": 10 })).await;
    }
    eprintln!("big export: {reads} tree reads, the slowest {worst:?}");
    pilot.ok("close", json!({})).await;

    let text = std::fs::read_to_string(&out).unwrap();
    assert_eq!(text.lines().count(), 1_000_001, "a header and every row");
    assert!(
        text.starts_with("n,code,label,ratio\n1,7,row 1,"),
        "{}",
        &text[..40]
    );
    assert!(
        reads > 1,
        "the export finished before the first read, so no read overlapped it"
    );
    assert!(
        worst <= MAX_STALL,
        "a tree read waited {worst:?} on the main thread during the export"
    );
}
