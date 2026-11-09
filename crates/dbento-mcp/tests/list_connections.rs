//! The server skeleton: list_connections, the fence, instructions and per-call workspace reads.
use std::sync::Arc;

use crate::common::{TestServer, text, unfence};
use dbento_core::MemoryStore;
use dbento_mcp::{DbentoServer, McpError, McpOptions};
use serde_json::{Value, json};

const THREE: &str = r#"dbento = 1
[folders."Prod"]
color = "red"
[folders."Prod/EU"]
[connections.lite]
engine = "sqlite"
path = "data/app.db"
read_only = true
[connections.shop]
engine = "postgres"
host = "db.internal"
database = "shop"
user = "app"
folder = "Prod/EU"
[connections.dev]
engine = "postgres"
host = "localhost"
database = "dev"
user = "me"
color = "green"
"#;

const ENFORCED: &str = "Read-only sessions on this connection are enforced by the database.";
const POSTGRES: &str = "Read-only sessions on this connection are enforced by the database: each \
    statement runs in `BEGIN READ ONLY`. The server still lets a few things through: functions \
    that open their own connections, such as `dblink_exec` where the dblink extension is \
    installed, and admin functions whose effect is not transactional, such as \
    `pg_terminate_backend`, `pg_cancel_backend` and `pg_stat_reset`. For a production database, \
    use a role without the privileges the rest need; it can still cancel or terminate backends of \
    its own role.";

// No server at these hosts: list_connections reads the workspace and never connects.
const MS: &str = r#"dbento = 1
[connections.bare]
engine = "mssql"
host = "db.invalid"
user = "app"
[connections.fenced]
engine = "sqlserver"
host = "db.invalid"
user = "app"
read_only_user = "dbento_reader"
"#;

/// The `connections` array sorted by name.
fn by_name(list: &Value) -> Vec<Value> {
    let mut v = list["connections"].as_array().expect("connections").clone();
    v.sort_by_key(|c| c["name"].as_str().expect("name").to_owned());
    v
}

#[tokio::test]
async fn lists_every_connection_with_engine_flags_and_folder() {
    let server = TestServer::start(THREE).await;
    let list = server.ok("list_connections", json!({})).await;
    assert_eq!(
        by_name(&list),
        vec![
            json!({"name": "dev", "engine": "postgres", "language": "SQL", "read_only": false, "read_only_enforcement": "database", "session": POSTGRES, "production": false, "folder": null}),
            json!({"name": "lite", "engine": "sqlite", "language": "SQL", "read_only": true, "read_only_enforcement": "database", "session": ENFORCED, "production": false, "folder": null}),
            json!({"name": "shop", "engine": "postgres", "language": "SQL", "read_only": false, "read_only_enforcement": "database", "session": POSTGRES, "production": true, "folder": "Prod/EU"}),
        ]
    );
}

#[tokio::test]
async fn an_unexposed_connection_is_not_listed() {
    let server = TestServer::start_with(THREE, |o| o.expose(["lite", "shop"])).await;
    let names: Vec<Value> = by_name(&server.ok("list_connections", json!({})).await)
        .into_iter()
        .map(|c| c["name"].clone())
        .collect();
    assert_eq!(names, vec![json!("lite"), json!("shop")]);
}

#[tokio::test]
async fn the_text_block_is_the_json_inside_a_fresh_nonce_fence() {
    let server = TestServer::start(THREE).await;
    let first = server.call("list_connections", json!({})).await;
    let second = server.call("list_connections", json!({})).await;
    let (n1, body) = unfence(&text(&first));
    let (n2, _) = unfence(&text(&second));
    assert_eq!(Some(body), first.structured_content);
    assert_eq!(n1.len(), 32, "{n1}");
    assert!(
        n1.chars()
            .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
        "{n1}"
    );
    assert_ne!(n1, n2, "the nonce is fresh per result");
}

#[tokio::test]
async fn instructions_say_results_are_untrusted() {
    let server = TestServer::start(THREE).await;
    let instructions = server.instructions().expect("instructions");
    assert!(instructions.contains("untrusted"), "{instructions}");
    assert!(
        instructions.contains("execute") && instructions.contains("approv"),
        "{instructions}"
    );
}

#[tokio::test]
async fn every_tool_declares_an_object_output_schema_and_honest_hints() {
    let server = TestServer::start(THREE).await;
    let tools = server.tools().await;
    assert!(tools.iter().any(|t| t.name == "execute"));
    for tool in tools {
        let schema = tool
            .output_schema
            .as_ref()
            .unwrap_or_else(|| panic!("{} has no outputSchema", tool.name));
        assert_eq!(schema.get("type"), Some(&json!("object")), "{}", tool.name);
        let hints = tool.annotations.as_ref().expect("annotations");
        if tool.name == "execute" {
            assert_eq!(hints.read_only_hint, Some(false));
            assert_eq!(hints.destructive_hint, Some(true));
        } else {
            assert_eq!(hints.read_only_hint, Some(true), "{}", tool.name);
        }
    }
}

#[tokio::test]
async fn the_workspace_is_read_again_on_every_call() {
    let server = TestServer::start(THREE).await;
    let added = format!("{THREE}[connections.extra]\nengine = \"sqlite\"\npath = \"x.db\"\n");
    std::fs::write(server.root().join("dbento.toml"), added).unwrap();
    assert_eq!(
        by_name(&server.ok("list_connections", json!({})).await).len(),
        4
    );
    std::fs::write(server.root().join("dbento.toml"), "not = [toml").unwrap();
    let result = server.call("list_connections", json!({})).await;
    assert_eq!(result.is_error, Some(true));
    let (_, body) = unfence(&text(&result));
    assert_eq!(
        body["error"]["code"], "workspace",
        "errors are fenced too: {body}"
    );
}

#[test]
fn a_folder_without_dbento_toml_does_not_start() {
    let empty = tempfile::TempDir::new().unwrap();
    let opened = DbentoServer::new(McpOptions::new(empty.path()), Arc::new(MemoryStore::new()));
    assert!(matches!(opened, Err(McpError::Open(_))));
}

#[tokio::test]
async fn a_sql_server_connection_states_its_level_without_connecting() {
    let server = TestServer::start(MS).await;
    let list = server.ok("list_connections", json!({})).await;
    let found = by_name(&list);
    assert_eq!(
        (&found[0]["engine"], &found[0]["read_only_enforcement"]),
        (&json!("mssql"), &json!("best_effort"))
    );
    let bare = found[0]["session"].as_str().unwrap();
    assert!(
        bare.starts_with("Read-only sessions on this connection are best effort"),
        "{bare}"
    );
    assert!(bare.contains("`read_only_user` is the answer"), "{bare}");
    assert_eq!(found[1]["read_only_enforcement"], "database");
    let fenced = found[1]["session"].as_str().unwrap();
    assert!(!fenced.contains("best effort"), "{fenced}");
}

#[tokio::test]
async fn the_tools_name_sql_server_where_they_list_engines() {
    let server = TestServer::start(MS).await;
    let tools = server.tools().await;
    let about = |name: &str| {
        let tool = tools.iter().find(|t| t.name == name).unwrap();
        tool.description.as_deref().unwrap().to_owned()
    };
    assert!(about("list_connections").contains("mssql"));
    assert!(about("list_schemas").contains("SQL Server"));
    assert!(about("explain").contains("SQL Server"));
    assert!(about("query").contains("one batch"));
    assert!(about("query").contains("extra_results"));
    assert!(server.instructions().unwrap().contains("`session`"));
}

// `mariadb` is an alias of the `mysql` engine, so it answers the same level and text.
#[tokio::test]
async fn a_mariadb_connection_states_what_the_server_still_lets_through() {
    let toml = r#"dbento = 1
[connections.maria]
engine = "mariadb"
host = "db.invalid"
user = "app"
"#;
    let server = TestServer::start(toml).await;
    let list = server.ok("list_connections", json!({})).await;
    let found = by_name(&list);
    assert_eq!(found[0]["read_only_enforcement"], "database");
    let text = found[0]["session"].as_str().unwrap();
    assert!(text.contains("`SELECT ... INTO OUTFILE`"), "{text}");
}

const SR: &str = r#"dbento = 1
[connections.graph]
engine = "surrealdb"
host = "db.invalid"
namespace = "app"
database = "main"
user = "reader"
auth_level = "database"
"#;

#[tokio::test]
async fn a_surrealdb_connection_states_its_language_and_residual_but_not_its_level() {
    let server = TestServer::start(SR).await;
    let list = server.ok("list_connections", json!({})).await;
    let found = by_name(&list);
    assert_eq!(
        (
            &found[0]["engine"],
            &found[0]["language"],
            &found[0]["read_only_enforcement"]
        ),
        (&json!("surrealdb"), &json!("SurrealQL"), &json!("unknown"))
    );
    let text = found[0]["session"].as_str().unwrap();
    for word in [
        "VIEWER",
        "after connecting",
        "`query`",
        "ALTER SYSTEM",
        "sequences",
    ] {
        assert!(text.contains(word), "{word}: {text}");
    }
}

#[tokio::test]
async fn the_tools_name_surrealdb_where_they_list_engines() {
    let server = TestServer::start(SR).await;
    let tools = server.tools().await;
    let about = |name: &str| {
        let tool = tools.iter().find(|t| t.name == name).unwrap();
        tool.description.as_deref().unwrap().to_owned()
    };
    assert!(about("list_connections").contains("surrealdb"));
    assert!(about("list_connections").contains("language"));
    assert!(about("query").contains("SurrealDB"));
}
