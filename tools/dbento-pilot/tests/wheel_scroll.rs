//! Wheel scrolling over the results grid, recorded with gpui-pilot's frame recorder in
//! headless and session mode, and judged against the spec's 144 Hz budget. A recording starts
//! only once the machine is quiet; a machine that stays loaded fails as too loaded to measure.
mod common;

use common::{Pilot, bounds, launch_args, rows_loaded, run_query, sqlite_workspace};
use serde_json::{Value, json};
use std::time::Duration;

/// A million typed rows without a table, so the grid keeps pulling on demand.
const MILLION: &str = "WITH RECURSIVE r(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM r WHERE n < 1000000) SELECT n, n * 7 % 1000 AS code, printf('row %d', n) AS label, n / 3.0 AS ratio FROM r";
/// Five thousand rows, parked at the default 1,000-row cap, so nothing streams meanwhile.
const FIVE_THOUSAND: &str = "WITH RECURSIVE r(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM r WHERE x < 5000) SELECT x FROM r";
/// Notches per scroll call (the protocol's most), calls per recording, and their spacing.
const NOTCHES: u64 = 200;
const CALLS: u64 = 3;
const NOTCH_MS: u64 = 30;
/// One notch is three lines of the 21 px line height the kit's scroll mask measures over
/// the grid.
const NOTCH_PX: f64 = 63.0;

/// Quiet is a 1-minute load average of at most a quarter task per CPU: recorded passes sat at
/// load 1.3 to 4 on the 16-CPU dev machine, misses at 7 and up.
const QUIET_LOAD_PER_CPU: f64 = 0.25;
/// A 1-minute average decays from load 20 to 4 in about 100 s once a build stops.
const QUIET_DEADLINE: Duration = Duration::from_secs(180);
/// How often the load is read again while waiting for quiet.
const QUIET_POLL: Duration = Duration::from_secs(5);

/// Launches in this file run one at a time, so a recording never shares the CPU with another.
static ONE_AT_A_TIME: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// The spec's pass criteria for a recording of `notches` wheel notches: one line per missed
/// criterion naming the number, empty when the scroll is smooth.
fn verdict(report: &Value, notches: u64, headless: bool) -> Vec<String> {
    let budget = report["budget_ms"].as_f64().expect("budget_ms");
    let mut failed = Vec::new();
    match report["intervals"]["p95_ms"].as_f64() {
        Some(p95) if p95 <= 1.5 * budget => {}
        Some(p95) => failed.push(format!("intervals.p95_ms {p95} > {:.3}", 1.5 * budget)),
        None => failed.push("no intervals".to_owned()),
    }
    let late = report["late"].as_u64().expect("late");
    if late as f64 > 0.02 * notches as f64 {
        failed.push(format!("late {late} > 2% of {notches} notches"));
    }
    match report["latency"]["p95_ms"].as_f64() {
        Some(p95) if p95 <= 2.0 * budget => {}
        Some(p95) => failed.push(format!("latency.p95_ms {p95} > {:.3}", 2.0 * budget)),
        None => failed.push("no latency".to_owned()),
    }
    if headless {
        let warnings = report["warnings"].as_array().cloned().unwrap_or_default();
        for name in ["unoptimised_host", "truncated"] {
            if warnings.contains(&json!(name)) {
                failed.push(format!("warning {name}"));
            }
        }
    }
    failed
}

/// One reading of how busy the machine is.
#[derive(Clone, Copy, Debug)]
struct Load {
    one_min: f64,
    cpus: usize,
}

/// What to do about a reading taken after waiting `waited` for quiet.
#[derive(Debug, PartialEq)]
enum Quiet {
    Ready,
    Wait,
    TooLoaded(String),
}

/// Record once the load per CPU is at most the quiet threshold; past the deadline, give up with
/// a message naming the load instead of letting a frame verdict blame the app.
fn quiet(load: Load, waited: Duration) -> Quiet {
    let per_cpu = load.one_min / load.cpus as f64;
    if per_cpu <= QUIET_LOAD_PER_CPU {
        Quiet::Ready
    } else if waited < QUIET_DEADLINE {
        Quiet::Wait
    } else {
        Quiet::TooLoaded(format!(
            "too loaded to measure: load {:.2} on {} CPUs ({per_cpu:.2} per CPU, quiet is {QUIET_LOAD_PER_CPU}), waited {} s",
            load.one_min,
            load.cpus,
            waited.as_secs()
        ))
    }
}

/// The 1-minute figure of a `/proc/loadavg` line.
fn parse_loadavg(text: &str) -> Option<f64> {
    text.split_whitespace().next()?.parse().ok()
}

fn read_load() -> Load {
    let text = std::fs::read_to_string("/proc/loadavg").expect("/proc/loadavg");
    Load {
        one_min: parse_loadavg(&text).expect("a 1-minute load average"),
        cpus: std::thread::available_parallelism().map_or(1, |n| n.get()),
    }
}

/// Holds a recording back until the machine is quiet, before anything is launched, so a load
/// spike from a build fails the test as unmeasurable rather than as a frame regression.
async fn wait_until_quiet() {
    let mut waited = Duration::ZERO;
    loop {
        let load = read_load();
        match quiet(load, waited) {
            Quiet::Ready => {
                eprintln!(
                    "quiet after {} s at load {:.2}",
                    waited.as_secs(),
                    load.one_min
                );
                return;
            }
            Quiet::Wait => {
                tokio::time::sleep(QUIET_POLL).await;
                waited += QUIET_POLL;
            }
            Quiet::TooLoaded(message) => panic!("{message}"),
        }
    }
}

fn centre(tree: &Value) -> Value {
    let grid = bounds(tree, "result-grid");
    json!({ "x": grid.x + grid.w / 2.0, "y": grid.y + grid.h / 2.0 })
}

/// Records the wheel-scroll script in `mode` and returns the `frames stop` report. Prints
/// the unedited report, the rows loaded around it and the close reply for the research note.
async fn record(mode: &str) -> Value {
    wait_until_quiet().await;
    let ws = sqlite_workspace();
    let pilot = Pilot::start().await;
    let launched = pilot.ok("launch", launch_args(mode, ws.path())).await;
    assert!(
        launched["refresh_hz"].as_f64().unwrap() > 143.0,
        "{launched}"
    );
    run_query(&pilot, MILLION, true).await;
    let at = centre(&pilot.ok("tree", json!({})).await);
    // Prime to about row 1,660, short of the first fetch boundary (2,048 rows at the first
    // park, pulled within 200 rows of the end), so the recording crosses one early.
    for _ in 0..3 {
        pilot
            .ok(
                "scroll",
                json!({ "target": at, "notches": NOTCHES, "notch_ms": 1, "settle_ms": 0 }),
            )
            .await;
    }
    pilot.ok("wait", json!({ "ms": 300 })).await;
    let before = rows_loaded(&pilot.ok("tree", json!({})).await);
    pilot.ok("frames", json!({ "action": "start" })).await;
    for _ in 0..CALLS {
        pilot
            .ok(
                "scroll",
                json!({ "target": at, "notches": NOTCHES, "notch_ms": NOTCH_MS, "settle_ms": 0 }),
            )
            .await;
    }
    pilot.ok("wait", json!({ "ms": 150 })).await;
    let report = pilot.ok("frames", json!({ "action": "stop" })).await;
    let after = rows_loaded(&pilot.ok("tree", json!({})).await);
    eprintln!("wheel-scroll {mode}: rows loaded {before} -> {after}\nframes stop: {report}");
    let closed = pilot.ok("close", json!({})).await;
    eprintln!("close: {closed}");
    pilot.assert_bus_untouched();
    assert!(
        after > before,
        "the recording crossed no fetch boundary: {before} -> {after}"
    );
    report
}

/// What holds of any honest recording, smooth or not: frames were drawn, nearly every notch
/// was answered by a frame, and a headless host was optimised.
fn assert_recorded(report: &Value, headless: bool) {
    let notches = NOTCHES * CALLS;
    assert!(report["frames"].as_u64().unwrap() >= 100, "{report}");
    let answered = report["latency"]["count"].as_u64().unwrap_or(0);
    assert!(
        answered * 100 >= notches * 95,
        "a frame followed only {answered} of {notches} notches: {report}"
    );
    if headless {
        assert_eq!(report["build"]["opt_level"], json!("3"), "{report}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn wheel_scroll_headless() {
    let _one = ONE_AT_A_TIME.lock().await;
    let report = record("headless").await;
    assert_recorded(&report, true);
    let failed = verdict(&report, NOTCHES * CALLS, true);
    assert!(
        failed.is_empty(),
        "headless scrolling missed the budget: {failed:?}\n{report}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn wheel_scroll_session() {
    let _one = ONE_AT_A_TIME.lock().await;
    let report = record("session").await;
    assert_recorded(&report, false);
    let failed = verdict(&report, NOTCHES * CALLS, false);
    assert!(
        failed.is_empty(),
        "session scrolling missed the budget: {failed:?}\n{report}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn one_notch_moves_the_grid_three_lines() {
    let _one = ONE_AT_A_TIME.lock().await;
    let ws = sqlite_workspace();
    let pilot = Pilot::start().await;
    pilot.ok("launch", launch_args("headless", ws.path())).await;
    run_query(&pilot, FIVE_THOUSAND, false).await;
    let tree = pilot.ok("tree", json!({})).await;
    let at = centre(&tree);
    let before = bounds(&tree, "cell-5-0").y;
    pilot
        .ok(
            "scroll",
            json!({ "target": at, "notches": 1, "settle_ms": 0 }),
        )
        .await;
    pilot.ok("wait", json!({ "ms": 300 })).await;
    let after = bounds(&pilot.ok("tree", json!({})).await, "cell-5-0").y;
    let moved = before - after;
    eprintln!("one notch moved the grid {moved} px");
    assert!(
        (moved - NOTCH_PX).abs() <= 1.0,
        "moved {moved} px, expected {NOTCH_PX}"
    );
    pilot.ok("close", json!({})).await;
}

/// The kit reveals its scrollbar when the offset changes; a notch must keep doing that.
#[tokio::test(flavor = "multi_thread")]
async fn a_wheel_notch_reveals_the_scrollbar() {
    let _one = ONE_AT_A_TIME.lock().await;
    let ws = sqlite_workspace();
    let shots = tempfile::tempdir().unwrap();
    let pilot = Pilot::start().await;
    pilot.ok("launch", launch_args("headless", ws.path())).await;
    run_query(&pilot, FIVE_THOUSAND, false).await;
    let at = centre(&pilot.ok("tree", json!({})).await);
    let grid_shot = |name: &str| {
        let path = shots.path().join(format!("{name}.png"));
        (
            json!({ "target": { "id": ["result-grid"] }, "save_to": &path }),
            path,
        )
    };
    // The bar fades out 2 s after the last scroll; first prove the region is otherwise static.
    pilot.ok("wait", json!({ "ms": 2500 })).await;
    let (args, quiet_a) = grid_shot("quiet-a");
    pilot.ok("screenshot", args).await;
    pilot.ok("wait", json!({ "ms": 300 })).await;
    let (args, quiet_b) = grid_shot("quiet-b");
    pilot.ok("screenshot", args).await;
    assert_eq!(
        std::fs::read(&quiet_a).unwrap(),
        std::fs::read(&quiet_b).unwrap(),
        "the grid changed with no input"
    );
    pilot
        .ok(
            "scroll",
            json!({ "target": at, "notches": 1, "settle_ms": 0 }),
        )
        .await;
    pilot.ok("wait", json!({ "ms": 300 })).await;
    let (args, shown) = grid_shot("shown");
    pilot.ok("screenshot", args).await;
    pilot.ok("wait", json!({ "ms": 3000 })).await;
    let (args, hidden) = grid_shot("hidden");
    pilot.ok("screenshot", args).await;
    assert_ne!(
        std::fs::read(&shown).unwrap(),
        std::fs::read(&hidden).unwrap(),
        "the notch did not reveal the scrollbar (same offset, bar shown vs faded)"
    );
    pilot.ok("close", json!({})).await;
}

fn report(intervals_p95: f64, late: u64, latency_p95: f64, warnings: &[&str]) -> Value {
    json!({
        "budget_ms": 6.944,
        "intervals": { "p95_ms": intervals_p95 },
        "late": late,
        "latency": { "p95_ms": latency_p95 },
        "warnings": warnings,
    })
}

#[test]
fn a_smooth_recording_passes() {
    assert!(verdict(&report(7.0, 0, 9.0, &[]), 600, true).is_empty());
}

#[test]
fn one_frame_per_notch_fails_on_intervals() {
    let failed = verdict(&report(30.1, 0, 9.0, &[]), 600, true);
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert!(failed[0].starts_with("intervals.p95_ms 30.1"), "{failed:?}");
}

#[test]
fn up_to_two_percent_late_notches_pass() {
    assert!(verdict(&report(7.0, 12, 9.0, &[]), 600, true).is_empty());
    let failed = verdict(&report(7.0, 13, 9.0, &[]), 600, true);
    assert!(failed[0].starts_with("late 13"), "{failed:?}");
}

#[test]
fn slow_notch_latency_fails() {
    let failed = verdict(&report(7.0, 0, 14.0, &[]), 600, true);
    assert_eq!(failed.len(), 1, "{failed:?}");
    assert!(failed[0].starts_with("latency.p95_ms 14"), "{failed:?}");
}

#[test]
fn headless_warnings_fail_and_session_ignores_them() {
    let noisy = report(7.0, 0, 9.0, &["unoptimised_host", "truncated"]);
    assert_eq!(
        verdict(&noisy, 600, true),
        ["warning unoptimised_host", "warning truncated"]
    );
    assert!(verdict(&noisy, 600, false).is_empty());
}

#[test]
fn an_empty_recording_fails() {
    let empty = json!({ "budget_ms": 6.944, "late": 0, "warnings": [] });
    assert_eq!(verdict(&empty, 600, true), ["no intervals", "no latency"]);
}

const IDLE: Load = Load {
    one_min: 2.0,
    cpus: 16,
};
const BUSY: Load = Load {
    one_min: 20.0,
    cpus: 16,
};

#[test]
fn a_quiet_machine_records_at_once() {
    assert_eq!(quiet(IDLE, Duration::ZERO), Quiet::Ready);
}

#[test]
fn the_threshold_itself_counts_as_quiet() {
    let edge = Load {
        one_min: 4.0,
        cpus: 16,
    };
    assert_eq!(quiet(edge, Duration::ZERO), Quiet::Ready);
    let over = Load {
        one_min: 4.1,
        cpus: 16,
    };
    assert_eq!(quiet(over, Duration::ZERO), Quiet::Wait);
}

#[test]
fn a_busy_machine_waits_until_the_deadline() {
    assert_eq!(quiet(BUSY, QUIET_DEADLINE - QUIET_POLL), Quiet::Wait);
}

#[test]
fn a_machine_still_busy_at_the_deadline_is_too_loaded_to_measure() {
    let Quiet::TooLoaded(message) = quiet(BUSY, QUIET_DEADLINE) else {
        panic!("expected too loaded");
    };
    assert_eq!(
        message,
        "too loaded to measure: load 20.00 on 16 CPUs (1.25 per CPU, quiet is 0.25), waited 180 s"
    );
}

#[test]
fn quiet_at_the_deadline_still_records() {
    assert_eq!(quiet(IDLE, QUIET_DEADLINE), Quiet::Ready);
}

#[test]
fn the_one_minute_figure_is_read_from_loadavg() {
    assert_eq!(parse_loadavg("2.10 1.37 0.73 3/1282 9913\n"), Some(2.10));
    assert_eq!(parse_loadavg(""), None);
}
