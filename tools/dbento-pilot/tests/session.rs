//! Dbento under gpui-pilot's session backend: the optimised app in a private KWin session,
//! driven through a query, with the harness's canary session bus left alone.
mod common;

use common::{Pilot, bounds, launch_args, rows_loaded, run_query, session_app, sqlite_workspace};
use serde_json::json;

#[tokio::test(flavor = "multi_thread")]
async fn the_optimised_app_runs_a_query_in_a_private_session() {
    let app = session_app();
    assert!(
        app.is_file(),
        "no optimised app at {}: run `cargo build -p dbento-app --bin dbento-app --profile timing`",
        app.display()
    );
    let ws = sqlite_workspace();
    let pilot = Pilot::start().await;
    let launched = pilot.ok("launch", launch_args("session", ws.path())).await;
    assert_eq!(launched["mode"], json!("session"));
    assert!(
        launched["refresh_hz"].as_f64().unwrap() > 143.0,
        "{launched}"
    );
    run_query(&pilot, "SELECT 1 AS one UNION ALL SELECT 2", true).await;
    let tree = pilot.ok("tree", json!({})).await;
    assert_eq!(rows_loaded(&tree), 2);
    let grid = bounds(&tree, "result-grid");
    assert!(grid.w > 200.0 && grid.h > 100.0, "{grid:?}");
    pilot.ok("close", json!({})).await;
    pilot.assert_bus_untouched();
}
