//! The server skeleton: list_connections, the fence, instructions and per-call workspace reads.
mod common;
use std::sync::Arc;

use common::{TestServer, text, unfence};
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
            json!({"name": "dev", "engine": "postgres", "read_only": false, "production": false, "folder": null}),
            json!({"name": "lite", "engine": "sqlite", "read_only": true, "production": false, "folder": null}),
            json!({"name": "shop", "engine": "postgres", "read_only": false, "production": true, "folder": "Prod/EU"}),
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
