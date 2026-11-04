//! SQL Server in the app under gpui-pilot: a connection made in the dialog tests, saves,
//! opens, runs a query and browses a table, and each read-only badge names its level.
mod common;
use std::path::Path;
use std::time::{Duration, Instant};

use common::{Pilot, bounds, find_node, launch_args};
use dbento_app::query_tab::{READ_ONLY_MSSQL_BEST_EFFORT_TOOLTIP, READ_ONLY_MSSQL_READER_TOOLTIP};
use serde_json::{Value, json};

const READER_SETUP: &str = "CREATE USER dbento_reader WITHOUT LOGIN; \
    ALTER ROLE db_datareader ADD MEMBER dbento_reader; \
    ALTER ROLE db_denydatawriter ADD MEMBER dbento_reader; \
    GRANT VIEW DEFINITION TO dbento_reader; \
    DENY ALTER ANY USER TO dbento_reader";

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
fn row_named(tree: &Value, aria: &str) -> Option<usize> {
    (0..500).find(|i| name(tree, &format!("tree-row-{i}")) == Some(aria))
}
/// Scrolls the explorer until the row labelled `aria` lies wholly inside the list.
async fn seek(pilot: &Pilot, aria: &str) -> (Value, usize) {
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let tree = pilot.ok("tree", json!({})).await;
        assert!(Instant::now() < deadline, "row {aria} never showed: {tree}");
        if find_node(&tree, "schema-tree").is_none() {
            pilot.ok("wait", json!({ "ms": 50 })).await;
            continue;
        }
        let list = bounds(&tree, "schema-tree");
        let notches = match row_named(&tree, aria) {
            None => 2,
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
async fn launch(ws: &Path, password: &str) -> Pilot {
    let pilot = Pilot::start().await;
    let mut args = launch_args("headless", ws);
    args["env"] = json!({ dbento_testkit::MS_SECRET_VAR: password });
    pilot.ok("launch", args).await;
    pilot
}
async fn run(pilot: &Pilot, conn: &str, sql: &str) {
    click(pilot, &format!("connection-{conn}"), json!({ "count": 2 })).await;
    pilot
        .ok("tree", json!({ "wait_for": { "id": ["editor"] } }))
        .await;
    pilot
        .ok(
            "type",
            json!({ "target": { "id": ["editor"] }, "text": sql }),
        )
        .await;
    click(pilot, "run-all", json!({})).await;
    until(pilot, "one row", |t| {
        name(t, "footer-rows") == Some("1 row")
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sql_server_connection_made_in_the_dialog_tests_saves_opens_and_browses() {
    dbento_testkit::skip_without_mssql!();
    let db = dbento_testkit::mssql_db().await;
    db.scalar("CREATE TABLE dbo.t (id int PRIMARY KEY, name nvarchar(10))")
        .await;
    db.scalar("INSERT INTO dbo.t VALUES (1, N'a'), (2, N'b')")
        .await;
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("dbento.toml"), "dbento = 1\n").unwrap();

    // Made through the dialog: the password comes from the variable, TLS is off.
    let pilot = launch(ws.path(), db.password()).await;
    click(&pilot, "connection-new", json!({})).await;
    until(&pilot, "the dialog", |t| {
        find_node(t, "conn-name").is_some()
    })
    .await;
    option(&pilot, "SQL Server").await;
    until(&pilot, "the SQL Server form", |t| {
        find_node(t, "conn-read-only-user").is_some()
    })
    .await;
    fill(&pilot, "conn-name", "ms").await;
    fill(&pilot, "conn-host", db.host()).await;
    fill(&pilot, "conn-port", &db.port().to_string()).await;
    fill(&pilot, "conn-database", db.database()).await;
    fill(&pilot, "conn-user", db.user()).await;
    option(&pilot, "Environment variable").await;
    until(&pilot, "the variable row", |t| {
        find_node(t, "conn-env").is_some()
    })
    .await;
    fill(&pilot, "conn-env", dbento_testkit::MS_SECRET_VAR).await;
    option(&pilot, "Off").await;
    click(&pilot, "conn-test", json!({})).await;
    let tree = until(&pilot, "the test answer", |t| {
        name(t, "conn-test-result").is_some_and(|s| s != "Testing\u{2026}")
    })
    .await;
    assert_eq!(name(&tree, "conn-test-result"), Some("Connected"));
    click(&pilot, "conn-save", json!({})).await;
    until(&pilot, "the dialog to close", |t| {
        find_node(t, "conn-dialog").is_none()
    })
    .await;
    let text = std::fs::read_to_string(ws.path().join("dbento.toml")).unwrap();
    assert!(text.contains("engine = \"mssql\""), "{text}");
    assert!(text.contains("tls = \"off\""), "{text}");
    assert!(text.contains(dbento_testkit::MS_SECRET_VAR), "{text}");

    // Opened: a query runs; the tree lists dbo's table and Open data reads both rows.
    run(&pilot, "ms", "SELECT name FROM dbo.t WHERE id = 1").await;
    click(&pilot, "connection-toggle-ms", json!({})).await;
    let (tree, ix) = seek(&pilot, "ms.dbo").await;
    if row_named(&tree, "ms.dbo.t").is_none() {
        click(&pilot, &format!("tree-toggle-{ix}"), json!({})).await;
    }
    let (_, ix) = seek(&pilot, "ms.dbo.t").await;
    click(
        &pilot,
        &format!("tree-row-{ix}"),
        json!({ "button": "right" }),
    )
    .await;
    until(&pilot, "the table menu", |t| {
        find_node(t, "tree-menu-open").is_some()
    })
    .await;
    click(&pilot, "tree-menu-open", json!({})).await;
    let tree = until(&pilot, "two rows", |t| {
        name(t, "footer-rows") == Some("2 rows")
    })
    .await;
    assert_eq!(
        name(&tree, "notice"),
        Some(
            "Opened as a plain query, so rows cannot be edited here: editing rows is not available on mssql yet"
        )
    );
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    pilot.assert_bus_untouched();
}

#[tokio::test(flavor = "multi_thread")]
async fn a_read_only_sql_server_badge_names_its_enforcement_level() {
    dbento_testkit::skip_without_mssql!();
    let db = dbento_testkit::mssql_db().await;
    db.scalar(READER_SETUP).await;
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(
        ws.path().join("dbento.toml"),
        format!(
            "dbento = 1\n{}{}",
            db.connection_toml("ro", "tls = \"off\"\nread_only = true"),
            db.connection_toml(
                "rr",
                "tls = \"off\"\nread_only = true\nread_only_user = \"dbento_reader\""
            )
        ),
    )
    .unwrap();
    for (conn, says) in [
        ("ro", READ_ONLY_MSSQL_BEST_EFFORT_TOOLTIP),
        ("rr", READ_ONLY_MSSQL_READER_TOOLTIP),
    ] {
        // Each in its own launch, so only one tab's badge is in the tree.
        let pilot = launch(ws.path(), db.password()).await;
        run(&pilot, conn, "SELECT 1 AS one").await;
        let tree = pilot.ok("tree", json!({})).await;
        assert_eq!(name(&tree, "read-only-badge"), Some(says), "{conn}");
        assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
        pilot.assert_bus_untouched();
    }
}
