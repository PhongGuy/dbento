//! The New connection dialog's database picker under gpui-pilot: a Postgres Test with
//! Database left empty offers the server's databases, a pick fills the field, and a change
//! to Host drops the list.
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
/// The text in the input under element `id`.
fn value<'a>(tree: &'a Value, id: &str) -> Option<&'a str> {
    tree["nodes"].as_array()?.iter().find_map(|n| {
        let inside = n["path"].as_array()?.contains(&json!(id));
        (inside && n["role"] == json!("TextInput")).then(|| n["value"].as_str())?
    })
}
/// The picker's rows in order, read by label until the first missing index.
fn offered(tree: &Value) -> Vec<String> {
    (0..)
        .map_while(|ix| name(tree, &format!("conn-database-suggestion-{ix}")))
        .map(str::to_owned)
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_tested_postgres_server_offers_its_databases_to_pick() {
    let db = dbento_testkit::pg_db().await;
    let kid = format!("{}_kid", db.database());
    let admin = db.admin().await;
    admin
        .batch_execute(&format!("CREATE DATABASE \"{kid}\""))
        .await
        .unwrap();
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("dbento.toml"), "dbento = 1\n").unwrap();
    let pilot = Pilot::start().await;
    let mut args = launch_args("headless", ws.path());
    args["env"] = json!({ dbento_testkit::SECRET_VAR: db.password() });
    pilot.ok("launch", args).await;

    // Postgres is the dialog's first engine; Database stays empty, so Test signs in to
    // `postgres` and lists the server from there.
    click(&pilot, "connection-new").await;
    until(&pilot, "the dialog", |t| {
        find_node(t, "conn-name").is_some()
    })
    .await;
    fill(&pilot, "conn-name", "pg").await;
    fill(&pilot, "conn-host", db.host()).await;
    fill(&pilot, "conn-port", &db.port().to_string()).await;
    fill(&pilot, "conn-user", db.user()).await;
    // The password comes from a variable: typed, it is scrubbed from every name in the tree,
    // and the fixture's password is spelled like the `postgres` database.
    pilot
        .ok(
            "click",
            json!({ "target": { "role": "Tab", "name": "Environment variable" } }),
        )
        .await;
    until(&pilot, "the variable row", |t| {
        find_node(t, "conn-env").is_some()
    })
    .await;
    fill(&pilot, "conn-env", dbento_testkit::SECRET_VAR).await;
    click(&pilot, "conn-test").await;
    let tree = until(&pilot, "the test answer", |t| {
        name(t, "conn-test-result").is_some_and(|s| s != "Testing\u{2026}")
    })
    .await;
    assert_eq!(name(&tree, "conn-test-result"), Some("Connected"));

    let tree = until(&pilot, "the database list", |t| {
        find_node(t, "conn-database-suggestions").is_some()
    })
    .await;
    let listed = offered(&tree);
    for want in [db.database(), kid.as_str()] {
        assert!(
            listed.iter().any(|d| d == want),
            "{want} missing: {listed:?}"
        );
    }
    assert_eq!(
        listed.last().map(String::as_str),
        Some("postgres"),
        "{listed:?}"
    );
    assert!(
        !listed.iter().any(|d| d.starts_with("template")),
        "templates offered: {listed:?}"
    );

    // A pick fills Database, closes the list and drops the Test answer it no longer matches.
    let ix = listed.iter().position(|d| *d == kid).unwrap();
    click(&pilot, &format!("conn-database-suggestion-{ix}")).await;
    let tree = until(&pilot, "the pick", |t| {
        find_node(t, "conn-database-suggestions").is_none()
    })
    .await;
    assert_eq!(value(&tree, "conn-database"), Some(kid.as_str()), "{tree}");
    assert!(find_node(&tree, "conn-test-result").is_none(), "{tree}");

    // Testing again offers the rest; a new Host means a different server, so the list goes.
    click(&pilot, "conn-test").await;
    until(&pilot, "the second list", |t| {
        find_node(t, "conn-database-suggestions").is_some()
    })
    .await;
    fill(&pilot, "conn-host", "x").await;
    until(&pilot, "the list to go", |t| {
        find_node(t, "conn-database-suggestions").is_none()
    })
    .await;

    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    pilot.assert_bus_untouched();
    admin
        .batch_execute(&format!("DROP DATABASE \"{kid}\" WITH (FORCE)"))
        .await
        .unwrap();
}
