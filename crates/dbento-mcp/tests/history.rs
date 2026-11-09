//! Every query that reached the database is appended to the history, marked as MCP.
use crate::common::TestServer;
use dbento_core::{History, HistoryEntry, HistoryFilter, HistoryOutcome, HistorySource};
use serde_json::json;

fn entries(server: &TestServer) -> Vec<HistoryEntry> {
    if !server.history_file().exists() {
        return Vec::new();
    }
    History::open(&server.history_file())
        .unwrap()
        .recent(&HistoryFilter::default(), 100)
}

#[tokio::test]
async fn queries_that_ran_are_recorded_as_mcp_and_refusals_are_not() {
    let server = TestServer::sqlite(
        "CREATE TABLE t(a INTEGER); INSERT INTO t VALUES (1), (2), (3);",
        "",
    )
    .await;
    server
        .ok(
            "query",
            json!({"connection": "lite", "sql": "SELECT a FROM t", "max_rows": 2}),
        )
        .await;
    server
        .err(
            "query",
            json!({"connection": "lite", "sql": "INSERT INTO t VALUES (4)"}),
        )
        .await;
    server
        .err(
            "query",
            json!({"connection": "lite", "sql": "SELECT 1; SELECT 2"}),
        )
        .await;
    server
        .err("query", json!({"connection": "nope", "sql": "SELECT 1"}))
        .await;
    server
        .ok("list_schemas", json!({"connection": "lite"}))
        .await;
    let e = entries(&server);
    assert_eq!(e.len(), 2, "{e:?}");
    // Newest first.
    assert_eq!(e[1].sql(), "SELECT a FROM t");
    assert_eq!(
        *e[1].outcome(),
        HistoryOutcome::Ok {
            rows: 2,
            truncated: true,
            rows_affected: None
        }
    );
    assert_eq!(e[0].sql(), "INSERT INTO t VALUES (4)");
    assert!(
        matches!(e[0].outcome(), HistoryOutcome::Failed { .. }),
        "{:?}",
        e[0].outcome()
    );
    let root = std::fs::canonicalize(server.root()).unwrap();
    for entry in &e {
        assert_eq!(entry.source(), HistorySource::Mcp);
        assert_eq!(entry.connection(), "lite");
        assert_eq!(entry.workspace(), root);
    }
    let text = std::fs::read_to_string(server.history_file()).unwrap();
    assert!(
        text.lines().all(|l| l.contains("\"source\":\"mcp\"")),
        "{text}"
    );
}
