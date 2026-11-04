//! The plan view under gpui-pilot on both engines. Set DBENTO_SHOTS_DIR to keep the shots.
mod common;
use std::path::Path;
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

async fn click(pilot: &Pilot, id: &str) {
    pilot.ok("click", json!({ "target": { "id": [id] } })).await;
}

async fn key(pilot: &Pilot, keys: &str) {
    pilot.ok("key", json!({ "keys": keys })).await;
}

async fn shot(pilot: &Pilot, name: &str) {
    if let Some(dir) = std::env::var_os("DBENTO_SHOTS_DIR").map(std::path::PathBuf::from) {
        std::fs::create_dir_all(&dir).unwrap();
        pilot
            .ok("screenshot", json!({ "save_to": dir.join(name) }))
            .await;
    }
}

async fn open_and_type(pilot: &Pilot, conn: &str, sql: &str) {
    pilot
        .ok(
            "click",
            json!({ "target": { "id": [format!("connection-{conn}")] }, "count": 2 }),
        )
        .await;
    pilot
        .ok("tree", json!({ "wait_for": { "id": ["editor"] } }))
        .await;
    pilot
        .ok(
            "type",
            json!({ "target": { "id": ["editor"] }, "text": sql }),
        )
        .await;
}

async fn lite_exec(ws: &Path, sql: &str) {
    let w = dbento_core::Workspace::open(ws).unwrap();
    let conn = w.connection("lite").unwrap();
    let mut s = dbento_core::connect(&w, conn, None, Default::default())
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
async fn sqlite_explain_shows_the_tree_then_the_raw_rows() {
    let ws = sqlite_workspace();
    lite_exec(
        ws.path(),
        "CREATE TABLE t(id INTEGER PRIMARY KEY, a INTEGER); CREATE INDEX t_a ON t(a); \
        INSERT INTO t VALUES (1, 10), (2, 20);",
    )
    .await;
    for theme in ["dark", "light"] {
        let pilot = Pilot::start().await;
        let mut args = launch_args("headless", ws.path());
        args["env"] = json!({ dbento_pilot::THEME_VAR: theme });
        pilot.ok("launch", args).await;
        open_and_type(&pilot, "lite", "SELECT * FROM t WHERE a = 10").await;
        key(&pilot, "ctrl-e").await;
        let tree = until(&pilot, "the plan tree", |t| {
            find_node(t, "plan-node-0").is_some()
        })
        .await;
        let first = find_node(&tree, "plan-node-0").unwrap()["name"]
            .as_str()
            .unwrap_or_default()
            .to_owned();
        assert!(first.starts_with("SEARCH t"), "{first}");
        assert!(find_node(&tree, "plan-header").is_some());
        shot(&pilot, &format!("plan-sqlite-tree-{theme}.png")).await;
        if theme == "dark" {
            click(&pilot, "plan-toggle-raw").await;
            until(&pilot, "the raw view", |t| {
                find_node(t, "plan-raw").is_some()
            })
            .await;
            shot(&pilot, "plan-sqlite-raw-dark.png").await;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn postgres_analyze_asks_runs_highlights_and_changes_nothing() {
    let db = dbento_testkit::pg_db().await;
    db.admin()
        .await
        .batch_execute("CREATE TABLE t(a int)")
        .await
        .unwrap();
    let ws = tempfile::tempdir().unwrap();
    std::fs::write(
        ws.path().join("dbento.toml"),
        format!("dbento = 1\n{}", db.connection_toml("pg", "")),
    )
    .unwrap();
    for theme in ["dark", "light"] {
        let pilot = Pilot::start().await;
        let mut args = launch_args("headless", ws.path());
        args["env"] =
            json!({ dbento_pilot::THEME_VAR: theme, dbento_testkit::SECRET_VAR: db.password() });
        pilot.ok("launch", args).await;
        open_and_type(
            &pilot,
            "pg",
            "INSERT INTO t SELECT g FROM generate_series(1, 50000) g",
        )
        .await;

        // Cancel on the ANALYZE question runs nothing: no plan, and the table stays empty.
        key(&pilot, "ctrl-shift-e").await;
        until(&pilot, "the ANALYZE question", |t| {
            find_node(t, "confirm-run").is_some()
        })
        .await;
        if theme == "dark" {
            shot(&pilot, "plan-pg-analyze-ask-dark.png").await;
        }
        click(&pilot, "confirm-skip").await;
        let tree = until(&pilot, "the question closing", |t| {
            find_node(t, "confirm-run").is_none()
        })
        .await;
        assert!(find_node(&tree, "plan-node-0").is_none(), "{tree}");
        assert_eq!(
            db.scalar("SELECT count(*)::text FROM t").await.as_deref(),
            Some("0")
        );

        key(&pilot, "ctrl-shift-e").await;
        until(&pilot, "the ANALYZE question", |t| {
            find_node(t, "confirm-run").is_some()
        })
        .await;
        click(&pilot, "confirm-run").await;
        let tree = until(&pilot, "the analyzed plan", |t| {
            find_node(t, "plan-node-0").is_some()
        })
        .await;
        // The ModifyTable root owns most of a large DELETE's execution, so it is the hot node.
        assert!(
            find_node(&tree, "plan-node-hot-0").is_some(),
            "the root is not marked hot: {tree}"
        );
        assert!(find_node(&tree, "plan-note").is_some());
        shot(&pilot, &format!("plan-pg-analyze-{theme}.png")).await;
        assert_eq!(
            db.scalar("SELECT count(*)::text FROM t").await.as_deref(),
            Some("0")
        );
    }
}
