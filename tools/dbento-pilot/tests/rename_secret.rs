//! QA under gpui-pilot: an outside rename carries a saved password, or names the variable.
mod common;

use std::time::{Duration, Instant};

use common::{Pilot, find_node, launch_args};
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

fn name<'a>(tree: &'a Value, id: &str) -> Option<&'a str> {
    find_node(tree, id).and_then(|n| n["name"].as_str())
}

async fn click(pilot: &Pilot, id: &str) {
    pilot.ok("click", json!({ "target": { "id": [id] } })).await;
}

async fn fill(pilot: &Pilot, id: &str, text: &str) {
    pilot
        .ok("type", json!({ "target": { "id": [id] }, "text": text }))
        .await;
}

async fn shot(pilot: &Pilot, file: &str) {
    if let Some(dir) = std::env::var_os("DBENTO_SHOTS_DIR").map(std::path::PathBuf::from) {
        std::fs::create_dir_all(&dir).unwrap();
        pilot
            .ok("screenshot", json!({ "save_to": dir.join(file) }))
            .await;
    }
}

// Opens `conn`'s tab, runs SELECT current_database() and waits for `want` in the first cell.
async fn database_is(pilot: &Pilot, conn: &str, want: &str) {
    common::open_connection(pilot, &format!("connection-{conn}")).await;
    pilot
        .ok("tree", json!({ "wait_for": { "id": ["editor"] } }))
        .await;
    fill(pilot, "editor", "SELECT current_database()").await;
    click(pilot, "run-all").await;
    until(pilot, "the database name", |t| {
        name(t, "cell-0-0") == Some(want)
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_renamed_connection_keeps_its_saved_password_after_a_question() {
    let db = dbento_testkit::pg_db().await;
    let moved = format!("{}_moved", db.database());
    let admin = db.admin().await;
    admin
        .batch_execute(&format!("CREATE DATABASE \"{moved}\""))
        .await
        .unwrap();
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("dbento.toml"), "dbento = 1\n").unwrap();
    let pilot = Pilot::start().await;
    pilot.ok("launch", launch_args("headless", ws.path())).await;

    // The password goes to the keychain (the pilot's in-memory store) through the dialog.
    click(&pilot, "connection-new").await;
    until(&pilot, "the dialog", |t| {
        find_node(t, "conn-name").is_some()
    })
    .await;
    fill(&pilot, "conn-name", "pg").await;
    fill(&pilot, "conn-host", db.host()).await;
    fill(&pilot, "conn-port", &db.port().to_string()).await;
    fill(&pilot, "conn-database", db.database()).await;
    fill(&pilot, "conn-user", db.user()).await;
    pilot
        .ok(
            "type",
            json!({ "target": { "id": ["conn-password"] }, "text": db.password(), "redact": true }),
        )
        .await;
    click(&pilot, "conn-save").await;
    until(&pilot, "the dialog to close", |t| {
        find_node(t, "conn-dialog").is_none()
    })
    .await;
    database_is(&pilot, "pg", db.database()).await;

    // A teammate's rename with another database on the same server and user; every other key
    // the dialog wrote stays as written.
    let toml = ws.path().join("dbento.toml");
    let saved = std::fs::read_to_string(&toml).unwrap();
    let renamed = saved
        .replace("[connections.pg]", "[connections.pg2]")
        .replace(
            &format!("database = \"{}\"", db.database()),
            &format!("database = \"{moved}\""),
        );
    assert!(
        renamed.contains("[connections.pg2]") && renamed.contains(&moved),
        "{renamed}"
    );
    std::fs::write(&toml, renamed).unwrap();
    until(&pilot, "the question", |t| {
        find_node(t, "rename-secret-move").is_some()
    })
    .await;
    shot(&pilot, "rename-secret-question.png").await;
    click(&pilot, "rename-secret-move").await;
    until(&pilot, "the question to close", |t| {
        find_node(t, "rename-secret-move").is_none()
    })
    .await;
    database_is(&pilot, "pg2", &moved).await;

    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    pilot.assert_bus_untouched();
    admin
        .batch_execute(&format!("DROP DATABASE \"{moved}\" WITH (FORCE)"))
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_renamed_connection_reading_a_variable_names_it() {
    let ws = tempfile::tempdir().unwrap();
    let conn = |n: &str| {
        format!(
            "dbento = 1\n\n[connections.{n}]\nengine = \"postgres\"\nhost = \"127.0.0.1\"\nport = 9\ndatabase = \"x\"\nuser = \"x\"\n"
        )
    };
    std::fs::write(ws.path().join("dbento.toml"), conn("pge")).unwrap();
    let pilot = Pilot::start().await;
    let mut args = launch_args("headless", ws.path());
    args["env"] = json!({ "DBENTO_SECRET_PGE": "unused" });
    pilot.ok("launch", args).await;
    until(&pilot, "the row", |t| {
        find_node(t, "connection-pge").is_some()
    })
    .await;

    std::fs::write(ws.path().join("dbento.toml"), conn("pge2")).unwrap();
    let tree = until(&pilot, "the notice", |t| {
        find_node(t, "rename-env-ok").is_some()
    })
    .await;
    let detail = name(&tree, "confirm-detail").unwrap_or_default();
    assert!(
        detail.contains("DBENTO_SECRET_PGE -> DBENTO_SECRET_PGE2"),
        "{detail}"
    );
    assert!(!detail.contains("unused"), "the value is never shown");
    shot(&pilot, "rename-env-notice.png").await;
    click(&pilot, "rename-env-ok").await;
    until(&pilot, "the notice to close", |t| {
        find_node(t, "rename-env-ok").is_none()
    })
    .await;
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    pilot.assert_bus_untouched();
}
