//! MongoDB in the app under gpui-pilot: a connection made in the dialog tests, saves and
//! opens; the tree and grid offer no SQL; a completed find shows its document; Copy as JSON
//! copies it; an empty-filter delete asks; each badge names the user's level.
mod common;
use std::path::Path;
use std::time::{Duration, Instant};

use common::{Pilot, bounds, find_node, launch_args};
use dbento_app::query_tab::{READ_ONLY_MONGO_BEST_EFFORT_TOOLTIP, READ_ONLY_MONGO_READER_TOOLTIP};
use dbento_testkit::{MG_SECRET_VAR, MgUser, mongo_db};
use serde_json::{Value, json};

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
    args["env"] = json!({ MG_SECRET_VAR: password });
    pilot.ok("launch", args).await;
    pilot
}
/// Opens a tab on `conn` and types `text` into its editor.
async fn type_in(pilot: &Pilot, conn: &str, text: &str) {
    click(pilot, &format!("connection-{conn}"), json!({ "count": 2 })).await;
    pilot
        .ok("tree", json!({ "wait_for": { "id": ["editor"] } }))
        .await;
    pilot
        .ok(
            "type",
            json!({ "target": { "id": ["editor"] }, "text": text }),
        )
        .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_mongodb_connection_made_in_the_dialog_shows_documents_and_no_sql() {
    let db = mongo_db().await;
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("dbento.toml"), "dbento = 1\n").unwrap();
    let pilot = launch(ws.path(), db.password(MgUser::Reader).unwrap()).await;

    // Made through the dialog: the URI names the reader, the password comes from the variable.
    click(&pilot, "connection-new", json!({})).await;
    until(&pilot, "the dialog", |t| {
        find_node(t, "conn-name").is_some()
    })
    .await;
    option(&pilot, "MongoDB").await;
    until(&pilot, "the MongoDB form", |t| {
        find_node(t, "conn-uri").is_some()
    })
    .await;
    fill(&pilot, "conn-name", "mg").await;
    fill(&pilot, "conn-uri", &db.uri(MgUser::Reader)).await;
    option(&pilot, "Environment variable").await;
    until(&pilot, "the variable row", |t| {
        find_node(t, "conn-env").is_some()
    })
    .await;
    fill(&pilot, "conn-env", MG_SECRET_VAR).await;
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
    for want in [
        "engine = \"mongodb\"".to_owned(),
        format!("uri = \"{}\"", db.uri(MgUser::Reader)),
        MG_SECRET_VAR.to_owned(),
    ] {
        assert!(text.contains(&want), "{want}: {text}");
    }

    // The tree lists the collection with no SQL actions; loading it also feeds completion.
    click(&pilot, "connection-toggle-mg", json!({})).await;
    let schema = format!("mg.{}", db.database());
    let collection = format!("{schema}.many");
    let (tree, ix) = seek(&pilot, &schema).await;
    if row_named(&tree, &collection).is_none() {
        click(&pilot, &format!("tree-toggle-{ix}"), json!({})).await;
    }
    let (_, ix) = seek(&pilot, &collection).await;
    click(
        &pilot,
        &format!("tree-row-{ix}"),
        json!({ "button": "right" }),
    )
    .await;
    let tree = until(&pilot, "the collection menu", |t| {
        find_node(t, "tree-menu-open").is_some()
    })
    .await;
    for id in [
        "tree-menu-count",
        "tree-menu-generate-insert",
        "tree-menu-copy-qualified",
    ] {
        assert!(find_node(&tree, id).is_none(), "{id}");
    }
    assert_eq!(
        name(&tree, "tree-menu-generate-select"),
        Some("Generate find")
    );
    pilot.ok("key", json!({ "keys": "escape" })).await;

    // Completion accepts the collection; the completed find returns one row.
    type_in(&pilot, "mg", "db.ma").await;
    pilot.ok("wait", json!({ "ms": 500 })).await;
    pilot.ok("key", json!({ "keys": "enter" })).await;
    pilot.ok("type", json!({ "text": ".find({_id: 7})" })).await;
    click(&pilot, "run-all", json!({})).await;
    until(&pilot, "one row", |t| {
        name(t, "footer-rows") == Some("1 row")
    })
    .await;

    // The record view shows the document; the grid copies it as JSON and offers no SQL copy.
    click(&pilot, "row-view-expand", json!({})).await;
    until(&pilot, "the mode row", |t| {
        find_node(t, "record-mode").is_some()
    })
    .await;
    option(&pilot, "Document").await;
    let tree = until(&pilot, "the document", |t| {
        find_node(t, "record-document").is_some()
    })
    .await;
    let shown = name(&tree, "record-document").unwrap_or_default();
    assert!(shown.contains("row 7") && shown.contains("_id"), "{shown}");
    click(&pilot, "row-view-expand", json!({})).await;
    until(&pilot, "the grid", |t| find_node(t, "cell-0-1").is_some()).await;
    click(&pilot, "cell-0-1", json!({ "button": "right" })).await;
    let tree = until(&pilot, "the grid menu", |t| {
        find_node(t, "grid-menu-copy-json").is_some()
    })
    .await;
    assert!(find_node(&tree, "grid-menu-copy-sql").is_none());
    click(&pilot, "grid-menu-copy-json", json!({})).await;
    let clip = pilot.ok("clipboard", json!({})).await;
    let copied: Value = serde_json::from_str(clip["text"].as_str().unwrap_or_default()).unwrap();
    assert_eq!(copied, json!([{"_id": 7, "n": 7, "s": "row 7"}]));
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    pilot.assert_bus_untouched();
}

#[tokio::test(flavor = "multi_thread")]
async fn badges_name_the_level_and_an_empty_filter_delete_asks() {
    let db = mongo_db().await;
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(
        ws.path().join("dbento.toml"),
        format!(
            "dbento = 1\n{}{}{}",
            db.connection_toml("rd", MgUser::Reader, "read_only = true"),
            db.connection_toml("wo", MgUser::Writer, "read_only = true"),
            db.connection_toml("wr", MgUser::Writer, "")
        ),
    )
    .unwrap();
    // Reader and writer share one password in the testkit. Each badge in its own launch, so
    // only one tab's badge is in the tree.
    let password = db.password(MgUser::Writer).unwrap();
    for (conn, says) in [
        ("rd", READ_ONLY_MONGO_READER_TOOLTIP),
        ("wo", READ_ONLY_MONGO_BEST_EFFORT_TOOLTIP),
    ] {
        let pilot = launch(ws.path(), password).await;
        type_in(&pilot, conn, "db.many.find({_id: 1})").await;
        click(&pilot, "run-all", json!({})).await;
        until(&pilot, "the badge", |t| {
            name(t, "read-only-badge") == Some(says)
        })
        .await;
        assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    }

    let pilot = launch(ws.path(), password).await;
    type_in(&pilot, "wr", "db.many.deleteMany({})").await;
    click(&pilot, "run-all", json!({})).await;
    let tree = until(&pilot, "the prompt", |t| {
        find_node(t, "confirm-detail").is_some()
    })
    .await;
    assert_eq!(
        name(&tree, "confirm-detail"),
        Some("Its filter is empty, so it deletes every document in the collection.")
    );
    click(&pilot, "confirm-skip", json!({})).await;
    until(&pilot, "the prompt to close", |t| {
        find_node(t, "confirm-detail").is_none()
    })
    .await;
    assert_eq!(db.count("many").await, 10_000);
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    pilot.assert_bus_untouched();
}
