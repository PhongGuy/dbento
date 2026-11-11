//! A parent with `databases = "all"` under gpui-pilot: an invalid name is dimmed without a
//! chevron, and a child expands and runs in its own database.
mod common;
use std::time::{Duration, Instant};

use common::{Pilot, bounds, find_node, launch_args};
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
fn row_named(tree: &Value, aria: &str) -> Option<usize> {
    (0..500).find(|i| name(tree, &format!("tree-row-{i}")) == Some(aria))
}
/// Scrolls the explorer until the row labelled `aria` lies wholly inside the list; a shared
/// server can list more databases than the virtualized tree draws at once.
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

#[tokio::test(flavor = "multi_thread")]
async fn a_parent_lists_its_databases_and_a_child_runs_in_its_own() {
    let db = dbento_testkit::pg_db().await;
    let kid = format!("{}_kid", db.database());
    let odd = format!("{}.odd", db.database());
    let admin = db.admin().await;
    for name in [&kid, &odd] {
        admin
            .batch_execute(&format!("CREATE DATABASE \"{name}\""))
            .await
            .unwrap();
    }
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(
        ws.path().join("dbento.toml"),
        format!(
            "dbento = 1\n{}",
            db.connection_toml("pg", "databases = \"all\"")
        ),
    )
    .unwrap();
    let pilot = Pilot::start().await;
    let mut args = launch_args("headless", ws.path());
    args["env"] = json!({ dbento_testkit::SECRET_VAR: db.password() });
    pilot.ok("launch", args).await;

    click(&pilot, "connection-toggle-pg", json!({})).await;
    let (tree, ix) = seek(&pilot, &format!("pg/{odd}:invalid")).await;
    assert!(
        find_node(&tree, &format!("tree-toggle-{ix}")).is_none(),
        "an invalid name cannot expand"
    );

    let child = format!("pg/{kid}");
    let (_, ix) = seek(&pilot, &child).await;
    click(&pilot, &format!("tree-toggle-{ix}"), json!({})).await;
    seek(&pilot, &format!("{child}.public")).await;
    let (_, ix) = seek(&pilot, &child).await;
    click(&pilot, &format!("tree-row-{ix}"), json!({ "count": 2 })).await;
    pilot
        .ok("tree", json!({ "wait_for": { "id": ["editor"] } }))
        .await;
    pilot
        .ok(
            "type",
            json!({ "target": { "id": ["editor"] }, "text": "SELECT current_database()" }),
        )
        .await;
    click(&pilot, "run-all", json!({})).await;
    until(&pilot, "the child's database", |t| {
        name(t, "cell-0-0") == Some(kid.as_str())
    })
    .await;
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    pilot.assert_bus_untouched();
    for name in [&kid, &odd] {
        admin
            .batch_execute(&format!("DROP DATABASE \"{name}\" WITH (FORCE)"))
            .await
            .unwrap();
    }
}
