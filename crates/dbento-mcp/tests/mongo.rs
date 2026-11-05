//! MongoDB through the MCP tools: documents beside rows, the syntax code, approval of a
//! write, and how the connection and the tools describe its language and level.
#![cfg(feature = "mongodb")]
mod common;
use common::TestServer;
use dbento_testkit::{MgUser, mongo_db};
use serde_json::{Value, json};

const MG: &str = "dbento = 1\n[connections.mg]\nengine = \"mongodb\"\n\
                  uri = \"mongodb://reader@db.invalid/app?authSource=admin\"\nread_only = true\n";

fn q(text: &str) -> Value {
    json!({"connection": "mg", "sql": text})
}

#[tokio::test]
async fn a_query_returns_each_rows_document_beside_its_cells() {
    let db = mongo_db().await;
    let server = TestServer::mongo(&db, MgUser::Reader, "").await;
    let r = server
        .ok("query", q("db.many.find({_id: {$lt: 2}}).sort({_id: 1})"))
        .await;
    assert_eq!(
        r["rows"],
        json!([["0", "0", "row 0"], ["1", "1", "row 1"]]),
        "{r}"
    );
    let docs: Vec<Value> = r["documents"]
        .as_array()
        .unwrap()
        .iter()
        .map(|d| serde_json::from_str(d.as_str().unwrap()).unwrap())
        .collect();
    assert_eq!(
        docs,
        [
            json!({"_id": 0, "n": 0, "s": "row 0"}),
            json!({"_id": 1, "n": 1, "s": "row 1"})
        ]
    );
    assert_eq!(r["read_only_enforcement"], "database", "{r}");
    let big = "db.many.aggregate([{$match: {_id: 0}}, {$project: {pad: {$range: [0, 3000]}}}])";
    let r = server.ok("query", q(big)).await;
    let cut = &r["documents"][0];
    assert_eq!(cut["cut"], true, "{r}");
    assert!(cut["text"].as_str().unwrap().len() <= 4096, "{r}");
}

#[tokio::test]
async fn javascript_is_a_syntax_error_and_a_write_waits_for_approval() {
    let db = mongo_db().await;
    let server = TestServer::mongo(&db, MgUser::Writer, "").await;
    let e = server.err("query", q("let x = 1")).await;
    assert_eq!(e["code"], "syntax", "{e}");
    let e = server
        .err("execute", q("db.many.deleteOne({_id: 1})"))
        .await;
    assert_eq!(e["code"], "approval_required", "{e}");
    assert_eq!(
        db.count("many").await,
        10_000,
        "nothing ran before approval"
    );
}

#[tokio::test]
async fn sql_results_carry_no_documents() {
    let server = TestServer::sqlite("CREATE TABLE t(x)", "").await;
    let r = server
        .ok("query", json!({"connection": "lite", "sql": "SELECT 1"}))
        .await;
    assert!(r.get("documents").is_none(), "{r}");
}

#[tokio::test]
async fn the_listing_and_the_tools_speak_of_the_connections_language() {
    let server = TestServer::start(MG).await;
    let list = server.ok("list_connections", json!({})).await;
    let c = &list["connections"][0];
    assert_eq!(
        (&c["engine"], &c["language"], &c["read_only_enforcement"]),
        (
            &json!("mongodb"),
            &json!("MongoDB shell"),
            &json!("unknown")
        )
    );
    let text = c["session"].as_str().unwrap();
    for word in [
        "privileges",
        "after connecting",
        "`database`",
        "`best_effort`",
    ] {
        assert!(text.contains(word), "{word}: {text}");
    }
    let tools = server.tools().await;
    let tool = |name: &str| tools.iter().find(|t| t.name == name).unwrap();
    let about = |name: &str| tool(name).description.as_deref().unwrap().to_owned();
    assert!(about("query").contains("query language") && about("query").contains("documents"));
    assert!(about("explain").contains("MongoDB"));
    assert!(about("list_saved_queries").contains(".js"));
    assert!(about("read_saved_query").contains(".js"));
    for name in ["query", "execute"] {
        let schema = serde_json::to_value(&*tool(name).input_schema).unwrap();
        let says = schema["properties"]["sql"]["description"]
            .as_str()
            .unwrap_or_default();
        assert!(says.contains("query language"), "{name}: {says}");
    }
}
