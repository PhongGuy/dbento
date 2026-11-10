//! DynamoDB through the MCP tools: the listing's language and level, the catalog, item
//! documents as plain JSON, and a write refused in the read-only session.
#![cfg(feature = "dynamodb")]
use crate::common::TestServer;
use dbento_testkit::dynamo_db;
use serde_json::{Value, json};

const DY: &str = "dbento = 1\n[connections.dy]\nengine = \"dynamodb\"\nregion = \"eu-west-1\"\n\
                  read_only = true\n";

#[tokio::test]
async fn the_listing_and_the_tools_speak_of_dynamodb() {
    let server = TestServer::start(DY).await;
    let list = server.ok("list_connections", json!({})).await;
    let c = &list["connections"][0];
    assert_eq!(
        (&c["engine"], &c["language"], &c["read_only_enforcement"]),
        (&json!("dynamodb"), &json!("PartiQL"), &json!("best_effort"))
    );
    let text = c["session"].as_str().unwrap();
    assert!(
        text.contains("IAM") && !text.contains("read_only_user"),
        "{text}"
    );
    let tools = server.tools().await;
    let about = |name: &str| {
        let t = tools.iter().find(|t| t.name == name).unwrap();
        t.description.as_deref().unwrap().to_owned()
    };
    let query = about("query");
    for word in ["DynamoDB", "full table scan", "consumed_capacity"] {
        assert!(query.contains(word), "{word}: {query}");
    }
    assert!(about("explain").contains("DynamoDB"));
    assert!(about("list_schemas").contains("region"));
}

#[tokio::test]
async fn the_catalog_and_a_query_answer_and_a_write_is_refused() {
    let db = dynamo_db().await;
    let server = TestServer::dynamo(&db, "").await;
    let schemas = server.ok("list_schemas", json!({"connection": "dy"})).await;
    assert_eq!(schemas["schemas"], json!([{"name": db.region()}]));
    let tables = server
        .ok(
            "list_tables",
            json!({"connection": "dy", "schema": db.region()}),
        )
        .await;
    let names: Vec<&str> = tables["tables"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["h", "orders", "types"]);
    let orders = server
        .ok(
            "describe_table",
            json!({"connection": "dy", "schema": db.region(), "table": "orders"}),
        )
        .await;
    assert_eq!(orders["columns"][0]["name"], "pk");
    assert_eq!(orders["columns"][0]["primary_key"], 1);

    let r = server
        .ok(
            "query",
            json!({"connection": "dy", "sql": "SELECT * FROM \"types\""}),
        )
        .await;
    assert_eq!(r["row_count"], 1);
    assert_eq!(r["read_only_enforcement"], "best_effort");
    assert!(
        r.get("consumed_capacity").is_none(),
        "Moto reports none: {r}"
    );
    let item: Value = serde_json::from_str(r["documents"][0].as_str().unwrap()).unwrap();
    assert_eq!(
        (&item["id"], &item["ss"]),
        (&json!("t1"), &json!(["x", "y"]))
    );

    let e = server
        .err(
            "query",
            json!({"connection": "dy", "sql": "INSERT INTO \"h\" VALUE {'id': 'mcp1'}"}),
        )
        .await;
    assert_eq!(e["code"], "read_only", "{e}");
    assert_eq!(db.count("h").await, 0);
}
