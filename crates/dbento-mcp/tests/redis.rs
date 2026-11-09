//! Redis through the MCP tools: the listing's language and level, the keyspace hint on the
//! table tools, the category refusals on `query`, and a flush waiting on `execute`.
#![cfg(feature = "redis")]
use crate::common::TestServer;
use dbento_testkit::{RdUser, redis_db};
use serde_json::{Value, json};

const RD: &str = "dbento = 1\n[connections.rd]\nengine = \"redis\"\nhost = \"db.invalid\"\n\
                  read_only = true\n";

fn q(text: &str) -> Value {
    json!({"connection": "rd", "sql": text})
}

#[tokio::test]
async fn the_listing_and_the_tools_speak_of_redis() {
    let server = TestServer::start(RD).await;
    let list = server.ok("list_connections", json!({})).await;
    let c = &list["connections"][0];
    assert_eq!(
        (&c["engine"], &c["language"], &c["read_only_enforcement"]),
        (&json!("redis"), &json!("Redis commands"), &json!("unknown"))
    );
    let text = c["session"].as_str().unwrap();
    for word in [
        "ACL",
        "after connecting",
        "`database`",
        "`best_effort`",
        "dbento_ro",
    ] {
        assert!(text.contains(word), "{word}: {text}");
    }
    for (tool, args) in [
        ("list_tables", json!({"connection": "rd", "schema": "db0"})),
        (
            "describe_table",
            json!({"connection": "rd", "schema": "db0", "table": "k"}),
        ),
    ] {
        let e = server.err(tool, args).await;
        assert_eq!(e["code"], "unsupported", "{tool}: {e}");
        let m = e["message"].as_str().unwrap();
        assert!(m.contains("SCAN") && m.contains("TYPE"), "{tool}: {m}");
    }
    let tools = server.tools().await;
    let about = |name: &str| {
        let t = tools.iter().find(|t| t.name == name).unwrap();
        t.description.as_deref().unwrap().to_owned()
    };
    assert!(about("explain").contains("Redis"));
    assert!(about("list_connections").contains("redis"));
    assert!(about("list_saved_queries").contains(".redis"));
    assert!(about("read_saved_query").contains(".redis"));
    let says = server.instructions().unwrap_or_default();
    assert!(says.contains(".redis") && says.contains("SCAN"), "{says}");
}

#[tokio::test]
async fn query_refuses_the_dangerous_categories_and_scan_runs() {
    let db = redis_db().await;
    let server = TestServer::redis(&db, RdUser::Reader, "read_only = true").await;
    for (text, category) in [
        ("KEYS *", "@dangerous"),
        ("INFO", "@dangerous"),
        ("CONFIG GET maxmemory", "@dangerous"),
        ("EVAL_RO \"return 1\" 0", "@scripting"),
        ("XREAD BLOCK 0 STREAMS mcpt:s $", "@blocking"),
        ("PUBSUB CHANNELS", "@pubsub"),
    ] {
        let e = server.err("query", q(text)).await;
        assert_eq!(e["code"], "refused_category", "{text}: {e}");
        let m = e["message"].as_str().unwrap();
        assert!(m.contains(category) && m.contains("SCAN"), "{text}: {m}");
    }
    let r = server.ok("query", q("SCAN 0 MATCH mcpt:* COUNT 10")).await;
    assert_eq!(r["read_only_enforcement"], "database", "{r}");
    let s = server.ok("list_schemas", json!({"connection": "rd"})).await;
    assert_eq!(s["schemas"][0]["name"], "db0", "{s}");
}

#[tokio::test]
async fn execute_skips_the_category_check_and_a_flush_waits_for_approval() {
    let db = redis_db().await;
    let server = TestServer::redis(&db, RdUser::Writer, "").await;
    let e = server.err("execute", q("FLUSHALL")).await;
    assert_eq!(e["code"], "approval_required", "{e}");
    let e = server.err("execute", q("GET mcpt:k")).await;
    assert_eq!(e["code"], "not_a_write", "{e}");
}
