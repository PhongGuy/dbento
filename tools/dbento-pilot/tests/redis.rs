//! Redis under gpui-pilot: dialog, key tree, value tabs, key filter, completion, no SQL,
//! the ACL badges and the flush prompt.
mod common;
use std::path::Path;
use std::time::{Duration, Instant};

use common::{Pilot, bounds, find_node, launch_args};
use dbento_app::query_tab::{READ_ONLY_REDIS_ACL_TOOLTIP, READ_ONLY_REDIS_BEST_EFFORT_TOOLTIP};
use dbento_testkit::{RD_SECRET_VAR, RdUser, RedisDb, redis_db};
use serde_json::{Value, json};

// Helpers copied from tests/mongo.rs, but `row_named` and `seek` also reach rows past 500 and
// above the view; `launch` passes the one Redis password as RD_SECRET_VAR.
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
/// The index of the rendered row labelled `aria`. Row ids carry the absolute row index, which
/// passes 500 once a folder of 1,200 keys is open, so every node is scanned rather than a range.
fn row_named(tree: &Value, aria: &str) -> Option<usize> {
    tree["nodes"].as_array()?.iter().find_map(|n| {
        let id = n["id"]
            .as_str()
            .or_else(|| n["path"].as_array()?.last()?.as_str())?;
        let ix = id.strip_prefix("tree-row-")?.parse().ok()?;
        (n["name"].as_str() == Some(aria)).then_some(ix)
    })
}
/// The lowest row index the explorer renders, which moves only while a scroll moves the list.
fn first_row(tree: &Value) -> Option<usize> {
    tree["nodes"]
        .as_array()?
        .iter()
        .filter_map(|n| {
            let id = n["id"]
                .as_str()
                .or_else(|| n["path"].as_array()?.last()?.as_str())?;
            id.strip_prefix("tree-row-")?.parse().ok()
        })
        .min()
}
/// Scrolls the explorer until the row labelled `aria` lies wholly inside the list. An unseen
/// row is sought downward first, then upward once a scroll stops moving the list.
async fn seek(pilot: &Pilot, aria: &str) -> (Value, usize) {
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut sweep = 2;
    let mut last = None;
    loop {
        let tree = pilot.ok("tree", json!({})).await;
        assert!(Instant::now() < deadline, "row {aria} never showed: {tree}");
        if find_node(&tree, "schema-tree").is_none() {
            pilot.ok("wait", json!({ "ms": 50 })).await;
            continue;
        }
        let list = bounds(&tree, "schema-tree");
        let notches = match row_named(&tree, aria) {
            None => {
                let first = first_row(&tree);
                if last.is_some() && first == last {
                    sweep = -sweep;
                }
                last = first;
                sweep
            }
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
    args["env"] = json!({ RD_SECRET_VAR: password });
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

/// Runs each line of `text` as the admin, split on spaces; a server error fails the test.
async fn seed(db: &RedisDb, text: &str) {
    for line in text.lines() {
        let v = db.cmd(&line.split(' ').collect::<Vec<_>>()).await;
        assert!(!format!("{v:?}").contains("ServerError"), "{line}: {v:?}");
    }
}

/// Clicks `part` (`tree-row`, `tree-toggle`) of the row named `aria`, with `extra`.
async fn row(pilot: &Pilot, aria: &str, part: &str, extra: Value) {
    let (_, ix) = seek(pilot, aria).await;
    click(pilot, &format!("{part}-{ix}"), extra).await;
}

/// Clicks db0's Load more until its scan ends (at most 20 pages); returns the settled tree.
async fn load_all(pilot: &Pilot) -> Value {
    for _ in 0..20 {
        let tree = until(pilot, "a settled key page", |t| {
            row_named(t, "rd.db0:loading").is_none()
        })
        .await;
        if row_named(&tree, "rd.db0:more").is_none() {
            return tree;
        }
        row(pilot, "rd.db0:more", "tree-row", json!({})).await;
    }
    panic!("db0's scan did not end in 20 pages");
}

/// Right-clicks `rd.db0` and waits for its menu.
async fn db_menu(pilot: &Pilot) -> Value {
    row(pilot, "rd.db0", "tree-row", json!({ "button": "right" })).await;
    until(pilot, "the db menu", |t| {
        find_node(t, "tree-menu-filter-keys").is_some()
    })
    .await
}

/// Double-clicks key `aria` and waits for its value tab's header to start with `head`.
async fn open_key(pilot: &Pilot, aria: &str, head: &str) {
    row(pilot, aria, "tree-row", json!({ "count": 2 })).await;
    until(pilot, head, |t| {
        name(t, "key-header").is_some_and(|h| h.starts_with(head))
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_redis_connection_made_in_the_dialog_browses_keys_and_values_and_offers_no_sql() {
    let db = redis_db().await;
    assert_eq!(
        db.index(),
        0,
        "the arias below name db0; point DBENTO_REDIS_URL at /0"
    );
    let pw = db.password(RdUser::Reader).unwrap();
    let list: String = (0..1_200).map(|i| format!(" {i}")).collect();
    let many: String = (0..1_200).map(|i| format!(" lm:{i:04} v")).collect();
    let lines = format!(
        "HSET pq:h a 1 b 2\nRPUSH pq:l{list}\nSET pq:s hello EX 600\nZADD pq:z 1 one 2 two"
    );
    seed(
        &db,
        &format!("{lines}\nSADD pq:set m\nXADD pq:x * f v\nMSET{many}"),
    )
    .await;
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(ws.path().join("dbento.toml"), "dbento = 1\n").unwrap();
    let pilot = launch(ws.path(), pw).await;

    // Made in the dialog: host, port, the reader, its secret from the variable.
    click(&pilot, "connection-new", json!({})).await;
    until(&pilot, "the dialog", |t| {
        find_node(t, "conn-name").is_some()
    })
    .await;
    option(&pilot, "Redis").await;
    until(&pilot, "the Redis form", |t| {
        find_node(t, "conn-database").is_some()
    })
    .await;
    fill(&pilot, "conn-name", "rd").await;
    fill(&pilot, "conn-host", db.host()).await;
    fill(&pilot, "conn-port", &db.port().to_string()).await;
    fill(&pilot, "conn-user", db.user(RdUser::Reader).unwrap()).await;
    option(&pilot, "Environment variable").await;
    fill(&pilot, "conn-env", RD_SECRET_VAR).await;
    click(&pilot, "conn-test", json!({})).await;
    let tree = until(&pilot, "the test", |t| {
        name(t, "conn-test-result").is_some_and(|r| r != "Testing\u{2026}")
    })
    .await;
    assert_eq!(name(&tree, "conn-test-result"), Some("Connected"));
    click(&pilot, "conn-save", json!({})).await;
    until(&pilot, "the dialog to close", |t| {
        find_node(t, "conn-dialog").is_none()
    })
    .await;
    let toml = std::fs::read_to_string(ws.path().join("dbento.toml")).unwrap();
    assert!(
        toml.contains("engine = \"redis\"") && toml.contains(RD_SECRET_VAR),
        "{toml}"
    );

    // The db row offers key actions and no SQL; its keys group by colon and page.
    click(&pilot, "connection-toggle-rd", json!({})).await;
    let tree = db_menu(&pilot).await;
    for id in [
        "tree-menu-count",
        "tree-menu-generate-insert",
        "tree-menu-copy-qualified",
    ] {
        assert!(find_node(&tree, id).is_none(), "{id}");
    }
    pilot.ok("key", json!({ "keys": "escape" })).await;
    row(&pilot, "rd.db0", "tree-toggle", json!({})).await;
    // The first 500-key page may hold no pq: key, so load every page first.
    let tree = load_all(&pilot).await;
    assert!(row_named(&tree, "rd.db0/pq:").is_some() && row_named(&tree, "rd.db0/lm:").is_some());
    row(&pilot, "rd.db0/lm:", "tree-toggle", json!({})).await;
    seek(&pilot, "rd.db0.lm:1199").await;
    row(&pilot, "rd.db0/lm:", "tree-toggle", json!({})).await;
    row(&pilot, "rd.db0/pq:", "tree-toggle", json!({})).await;
    let (tree, ix) = seek(&pilot, "rd.db0.pq:s").await;
    let detail = name(&tree, &format!("tree-detail-{ix}")).unwrap_or_default();
    assert!(
        detail.starts_with("string ") && detail.ends_with('m'),
        "{detail}"
    );

    // Each kind opens in its own value tab; a long list pages with Load more.
    for (k, head) in [
        ("h", "hash \u{B7} 2 fields"),
        ("s", "string \u{B7} 5 bytes \u{B7} expires in"),
        ("z", "zset \u{B7} 2 members"),
        ("set", "set \u{B7} 1 member"),
        ("x", "stream \u{B7} 1 entry"),
        ("l", "list \u{B7} 1200 elements"),
    ] {
        open_key(&pilot, &format!("rd.db0.pq:{k}"), head).await;
    }
    for _ in 0..2 {
        until(&pilot, "Load more", |t| {
            find_node(t, "key-load-more").is_some()
        })
        .await;
        click(&pilot, "key-load-more", json!({})).await;
        pilot.ok("wait", json!({ "ms": 300 })).await;
    }
    until(&pilot, "the list's end", |t| {
        find_node(t, "key-load-more").is_none()
    })
    .await;
    click(&pilot, "cell-0-1", json!({ "button": "right" })).await;
    let tree = until(&pilot, "the grid menu", |t| {
        find_node(t, "grid-menu-copy-json").is_some()
    })
    .await;
    assert!(find_node(&tree, "grid-menu-copy-sql").is_none());
    pilot.ok("key", json!({ "keys": "escape" })).await;

    // Filter keys narrows db0 to hashes under pq:.
    db_menu(&pilot).await;
    click(&pilot, "tree-menu-filter-keys", json!({})).await;
    until(&pilot, "the filter ask", |t| {
        find_node(t, "save-name").is_some()
    })
    .await;
    // The ask opens with its text selected, so typing replaces it.
    fill(&pilot, "save-name", "pq:* type:hash").await;
    click(&pilot, "save-ok", json!({})).await;
    let tree = load_all(&pilot).await;
    if row_named(&tree, "rd.db0.pq:h").is_none() {
        row(&pilot, "rd.db0/pq:", "tree-toggle", json!({})).await;
    }
    let tree = until(&pilot, "only the hash", |t| {
        row_named(t, "rd.db0.pq:h").is_some()
    })
    .await;
    assert!(row_named(&tree, "rd.db0.pq:l").is_none() && row_named(&tree, "rd.db0/lm:").is_none());

    // The editor completes a command and runs it; it offers neither Format nor explain.
    // The first word asks for COMMAND DOCS, which land after its popup; the next letter reopens it.
    type_in(&pilot, "rd", "HGET").await;
    pilot.ok("wait", json!({ "ms": 1_000 })).await;
    pilot.ok("type", json!({ "text": "A" })).await;
    pilot.ok("wait", json!({ "ms": 500 })).await;
    pilot.ok("key", json!({ "keys": "enter" })).await;
    pilot.ok("type", json!({ "text": " pq:h" })).await;
    click(&pilot, "run-all", json!({})).await;
    until(&pilot, "two rows", |t| {
        name(t, "footer-rows") == Some("2 rows")
    })
    .await;
    click(&pilot, "editor", json!({ "button": "right" })).await;
    let tree = until(&pilot, "the editor menu", |t| {
        find_node(t, "editor-menu-run").is_some()
    })
    .await;
    assert!(
        find_node(&tree, "editor-menu-format").is_none()
            && find_node(&tree, "editor-menu-explain").is_none()
    );
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    pilot.assert_bus_untouched();
}

#[tokio::test(flavor = "multi_thread")]
async fn badges_name_the_acl_level_and_a_flush_asks() {
    let db = redis_db().await;
    seed(&db, "SET pb:k kept").await;
    let pw = db.password(RdUser::Reader).unwrap();
    let ws = tempfile::tempdir().unwrap();
    let ro = "read_only = true";
    let (rd, wo) = (
        db.connection_toml("rd", RdUser::Reader, ro),
        db.connection_toml("wo", RdUser::Writer, ro),
    );
    let toml = format!(
        "dbento = 1\n{rd}{wo}{}",
        db.connection_toml("wr", RdUser::Writer, "")
    );
    std::fs::write(ws.path().join("dbento.toml"), toml).unwrap();
    // Each badge in its own launch, so only one tab's badge is in the tree.
    for (c, says) in [
        ("rd", READ_ONLY_REDIS_ACL_TOOLTIP),
        ("wo", READ_ONLY_REDIS_BEST_EFFORT_TOOLTIP),
    ] {
        let pilot = launch(ws.path(), pw).await;
        type_in(&pilot, c, "GET pb:k").await;
        click(&pilot, "run-all", json!({})).await;
        until(&pilot, "the badge", |t| {
            name(t, "read-only-badge").is_some_and(|b| b.starts_with(says))
        })
        .await;
        assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    }
    let pilot = launch(ws.path(), pw).await;
    type_in(&pilot, "wr", "FLUSHDB").await;
    click(&pilot, "run-all", json!({})).await;
    let tree = until(&pilot, "the prompt", |t| {
        find_node(t, "confirm-detail").is_some()
    })
    .await;
    let says = "It deletes every key in the database, or with FLUSHALL in every database.";
    assert_eq!(name(&tree, "confirm-detail"), Some(says));
    click(&pilot, "confirm-skip", json!({})).await;
    until(&pilot, "the prompt to close", |t| {
        find_node(t, "confirm-detail").is_none()
    })
    .await;
    assert_eq!(
        db.text(&["GET", "pb:k"]).await.as_deref(),
        Some("kept"),
        "the flush never ran"
    );
    assert_eq!(pilot.ok("close", json!({})).await["exit_code"], json!(0));
    pilot.assert_bus_untouched();
}
