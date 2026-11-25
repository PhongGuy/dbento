//! MySQL in the app under gpui-pilot: a read-only refusal, the tree, open data, an edit committed,
//! a foreign key followed, and EXPLAIN analyzing a read while only planning a write.
mod common;
use std::path::Path;
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
/// The visible index of the explorer row whose aria label is `aria`.
fn row_named(tree: &Value, aria: &str) -> Option<usize> {
    (0..500).find(|i| name(tree, &format!("tree-row-{i}")) == Some(aria))
}
/// Scrolls the explorer until the row labelled `aria` lies wholly inside the list, since a
/// shared gate server can hold more databases than the virtualized tree draws at once and a
/// click aims at the row's centre, which a clipped edge row may not show.
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
        // Down two notches while the row is undrawn, then one notch toward a clipped row.
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
async fn launch(ws: &Path, password: &str) -> Pilot {
    let pilot = Pilot::start().await;
    let mut args = launch_args("headless", ws);
    args["env"] = json!({ dbento_testkit::MY_SECRET_VAR: password });
    pilot.ok("launch", args).await;
    pilot
}
async fn my_exec(ws: &Path, password: &str, sql: &str) {
    let w = dbento_core::Workspace::open(ws).unwrap();
    let conn = w.declared("my").unwrap();
    let secret = Some(dbento_core::Secret::new(password));
    let mut s = dbento_core::connect(&w, conn, secret, Default::default())
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

#[tokio::test(flavor = "multi_thread")]
async fn mysql_refuses_a_read_only_write_then_browses_and_opens_a_table() {
    let db = dbento_testkit::mysql_db().await;
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(
        ws.path().join("dbento.toml"),
        format!(
            "dbento = 1\n{}{}",
            db.connection_toml("my", ""),
            db.connection_toml("ro", "read_only = true")
        ),
    )
    .unwrap();
    my_exec(
        ws.path(),
        db.password(),
        "CREATE TABLE t (id INT PRIMARY KEY, name VARCHAR(10)); INSERT INTO t VALUES (1, 'a'), (2, 'b')",
    )
    .await;

    // A write on the read-only connection fails in the database and changes nothing.
    let pilot = launch(ws.path(), db.password()).await;
    common::open_connection(&pilot, "connection-ro").await;
    pilot
        .ok("tree", json!({ "wait_for": { "id": ["editor"] } }))
        .await;
    pilot
        .ok(
            "type",
            json!({ "target": { "id": ["editor"] }, "text": "INSERT INTO t VALUES (3, 'c')" }),
        )
        .await;
    click(&pilot, "run-all", json!({})).await;
    let tree = until(&pilot, "the refusal", |t| {
        find_node(t, "result-error").is_some()
    })
    .await;
    let says = name(&tree, "result-error-message").unwrap_or_default();
    assert!(says.starts_with("read-only session:"), "{says}");
    assert_eq!(
        db.scalar("SELECT COUNT(*) FROM t").await.as_deref(),
        Some("2")
    );
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    pilot.assert_bus_untouched();

    // The tree lists the database and its table; Open data runs the SELECT with a plain-query note.
    let pilot = launch(ws.path(), db.password()).await;
    click(&pilot, "connection-toggle-my", json!({})).await;
    let schema = format!("my.{}", db.database());
    let table = format!("{schema}.t");
    let (tree, ix) = seek(&pilot, &schema).await;
    if row_named(&tree, &table).is_none() {
        click(&pilot, &format!("tree-toggle-{ix}"), json!({})).await;
    }
    let (_, ix) = seek(&pilot, &table).await;
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
    assert!(find_node(&tree, "browse-not-editable").is_none(), "{tree}");
    assert!(find_node(&tree, "notice").is_none(), "{tree}");
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    pilot.assert_bus_untouched();
}

#[tokio::test(flavor = "multi_thread")]
async fn mysql_edits_a_row_commits_and_follows_a_foreign_key() {
    let db = dbento_testkit::mysql_db().await;
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(
        ws.path().join("dbento.toml"),
        format!("dbento = 1\n{}", db.connection_toml("my", "")),
    )
    .unwrap();
    my_exec(
        ws.path(),
        db.password(),
        "CREATE TABLE parent (id INT PRIMARY KEY, name VARCHAR(10)); \
         CREATE TABLE child (id INT PRIMARY KEY, parent_id INT, note VARCHAR(10), \
           FOREIGN KEY (parent_id) REFERENCES parent (id)); \
         INSERT INTO parent VALUES (1, 'p1'), (2, 'p2'); INSERT INTO child VALUES (1, 2, 'a'), (2, 1, 'b')",
    )
    .await;
    let pilot = launch(ws.path(), db.password()).await;
    click(&pilot, "connection-toggle-my", json!({})).await;
    let schema = format!("my.{}", db.database());
    let table = format!("{schema}.child");
    let (tree, ix) = seek(&pilot, &schema).await;
    if row_named(&tree, &table).is_none() {
        click(&pilot, &format!("tree-toggle-{ix}"), json!({})).await;
    }
    let (_, ix) = seek(&pilot, &table).await;
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
    assert!(find_node(&tree, "browse-not-editable").is_none(), "{tree}");

    // Edit a note from the keyboard, save, and commit.
    click(&pilot, "cell-0-2", json!({})).await;
    pilot.ok("key", json!({ "keys": "enter" })).await;
    until(&pilot, "the cell editor", |t| {
        find_node(t, "cell-editor").is_some()
    })
    .await;
    pilot.ok("type", json!({ "text": "z" })).await;
    pilot.ok("key", json!({ "keys": "enter" })).await;
    until(&pilot, "one pending change", |t| {
        name(t, "pending-changes") == Some("1 pending change")
    })
    .await;
    click(&pilot, "save-edits", json!({})).await;
    until(&pilot, "the prompt", |t| {
        find_node(t, "apply-commit").is_some()
    })
    .await;
    click(&pilot, "apply-commit", json!({})).await;
    until(&pilot, "the committed value", |t| {
        name(t, "cell-0-2") == Some("z") && find_node(t, "pending-changes").is_none()
    })
    .await;
    assert_eq!(
        db.scalar("SELECT note FROM child WHERE id = 1")
            .await
            .as_deref(),
        Some("z")
    );

    // The row's outgoing key opens the parent it names.
    click(&pilot, "inspector-toggle", json!({})).await;
    until(&pilot, "the inspector", |t| {
        find_node(t, "inspector-title").is_some() || find_node(t, "inspector-empty").is_some()
    })
    .await;
    click(&pilot, "cell-0-1", json!({})).await;
    until(&pilot, "the outgoing link", |t| {
        name(t, "ref-out-0") == Some("parent id = 2")
    })
    .await;
    click(&pilot, "ref-out-0", json!({})).await;
    until(&pilot, "parent filtered to id 2", |t| {
        name(t, "footer-rows") == Some("1 row") && name(t, "cell-0-1") == Some("p2")
    })
    .await;
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    pilot.assert_bus_untouched();
}

#[tokio::test(flavor = "multi_thread")]
async fn mysql_analyzes_a_read_and_only_plans_a_write() {
    let db = dbento_testkit::mysql_db().await;
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(
        ws.path().join("dbento.toml"),
        format!("dbento = 1\n{}", db.connection_toml("my", "")),
    )
    .unwrap();
    my_exec(
        ws.path(),
        db.password(),
        "CREATE TABLE t (id INT PRIMARY KEY, a INT); INSERT INTO t VALUES (1, 10), (2, 20)",
    )
    .await;
    let pilot = launch(ws.path(), db.password()).await;
    common::open_connection(&pilot, "connection-my").await;
    pilot
        .ok("tree", json!({ "wait_for": { "id": ["editor"] } }))
        .await;
    pilot
        .ok(
            "type",
            json!({ "target": { "id": ["editor"] },
                    "text": "UPDATE t SET a = a + 1 WHERE id = 1;\nSELECT * FROM t WHERE a > 5" }),
        )
        .await;

    // The caret sits in the SELECT: ANALYZE runs it unasked and shows its timings, no note.
    pilot.ok("key", json!({ "keys": "ctrl-shift-e" })).await;
    let tree = until(&pilot, "the analyzed read", |t| {
        name(t, "plan-node-0").is_some_and(|n| n.starts_with("Filter"))
    })
    .await;
    assert!(find_node(&tree, "confirm-run").is_none(), "{tree}");
    assert!(find_node(&tree, "plan-note").is_none(), "{tree}");

    // ANALYZE of the UPDATE asks nothing, shows its plan with the plan-only note, and runs nothing.
    pilot.ok("key", json!({ "keys": "ctrl-home" })).await;
    pilot.ok("key", json!({ "keys": "ctrl-shift-e" })).await;
    let tree = until(&pilot, "the write's plan", |t| {
        (0..8).any(|i| name(t, &format!("plan-node-{i}")).is_some_and(|n| n.starts_with("Update")))
    })
    .await;
    assert!(find_node(&tree, "confirm-run").is_none(), "{tree}");
    assert!(find_node(&tree, "plan-note").is_some(), "{tree}");
    assert_eq!(
        db.scalar("SELECT CAST(a AS CHAR) FROM t WHERE id = 1")
            .await
            .as_deref(),
        Some("10")
    );
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    pilot.assert_bus_untouched();
}
