//! The connection dialog's engine row driven by keys in the running app: Tab from Name
//! reaches it, and Right, Home, End and Left swap the form between Postgres, SQLite, MySQL,
//! DuckDB and SQL Server.
//! Set DBENTO_SHOTS_DIR to keep a shot of the focused row.
mod common;

use std::time::{Duration, Instant};

use common::{Pilot, find_node, launch_args, sqlite_workspace};
use serde_json::{Value, json};

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

async fn key(pilot: &Pilot, keys: &str) {
    pilot.ok("key", json!({ "keys": keys })).await;
}

/// The file forms show a File field and no Host, the server forms the reverse, and only
/// MySQL's and DuckDB's carry the read-only help line; only SQL Server's has a reader user.
fn sqlite_form(tree: &Value) -> bool {
    file_form(tree) && find_node(tree, "conn-read-only-help").is_none()
}

fn duckdb_form(tree: &Value) -> bool {
    file_form(tree) && find_node(tree, "conn-read-only-help").is_some()
}

fn file_form(tree: &Value) -> bool {
    find_node(tree, "conn-path").is_some() && find_node(tree, "conn-host").is_none()
}

fn postgres_form(tree: &Value) -> bool {
    server_form(tree)
        && find_node(tree, "conn-read-only-help").is_none()
        && find_node(tree, "conn-read-only-user").is_none()
}

fn mysql_form(tree: &Value) -> bool {
    server_form(tree)
        && find_node(tree, "conn-read-only-help").is_some()
        && find_node(tree, "conn-read-only-user").is_none()
}

fn mssql_form(tree: &Value) -> bool {
    server_form(tree) && find_node(tree, "conn-read-only-user").is_some()
}

fn server_form(tree: &Value) -> bool {
    find_node(tree, "conn-host").is_some() && find_node(tree, "conn-path").is_none()
}

#[tokio::test(flavor = "multi_thread")]
async fn the_engine_row_is_reached_by_tab_and_driven_by_arrows() {
    let ws = sqlite_workspace();
    let pilot = Pilot::start().await;
    pilot.ok("launch", launch_args("headless", ws.path())).await;
    pilot
        .ok("click", json!({ "target": { "id": ["connection-new"] } }))
        .await;
    until(&pilot, "the dialog", postgres_form).await;
    pilot
        .ok("click", json!({ "target": { "id": ["conn-name"] } }))
        .await;

    // Tab leaves Name for the engine row; from here on only keys touch the dialog.
    key(&pilot, "tab").await;
    key(&pilot, "right").await;
    until(&pilot, "Right picked SQLite", sqlite_form).await;
    if let Some(dir) = std::env::var_os("DBENTO_SHOTS_DIR").map(std::path::PathBuf::from) {
        std::fs::create_dir_all(&dir).unwrap();
        pilot
            .ok(
                "screenshot",
                json!({ "save_to": dir.join("segmented-focus.png") }),
            )
            .await;
    }
    key(&pilot, "home").await;
    until(&pilot, "Home picked Postgres", postgres_form).await;
    key(&pilot, "end").await;
    until(&pilot, "End picked SQL Server", mssql_form).await;
    key(&pilot, "left").await;
    until(&pilot, "Left picked DuckDB", duckdb_form).await;
    key(&pilot, "left").await;
    until(&pilot, "Left again picked MySQL", mysql_form).await;
    key(&pilot, "left").await;
    until(&pilot, "Left a third time picked SQLite", sqlite_form).await;
    key(&pilot, "left").await;
    until(&pilot, "Left a fourth time picked Postgres", postgres_form).await;

    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    pilot.assert_bus_untouched();
}
