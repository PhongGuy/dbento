//! The MCP tools on MySQL and MariaDB: read-only by the database, transaction control refused,
//! writes filed with their unguarded kinds, and the timeout and a client cancel stop the
//! statement on the server.
mod common;
use std::time::Duration;

use common::TestServer;
use dbento_core::Unguarded;
use dbento_testkit::{MyDb, my_matrix};
use serde_json::{Value, json};

fn q(sql: &str) -> Value {
    json!({"connection": "my", "sql": sql})
}

async fn until(db: &MyDb, sql: &str, want: &str) -> bool {
    for _ in 0..50 {
        if db.scalar(sql).await.as_deref() == Some(want) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

#[tokio::test]
async fn the_tools_refuse_file_and_time_out_as_on_postgres() {
    for (label, db) in my_matrix().await {
        db.scalar("CREATE TABLE t (a INT)").await;
        db.scalar("INSERT INTO t VALUES (1), (2)").await;
        let server = TestServer::mysql(&db, "statement_timeout = \"500ms\"").await;
        let r = server
            .ok("query", q("SELECT CONCAT('v', a) AS v FROM t ORDER BY a"))
            .await;
        assert_eq!(r["rows"], json!([["v1"], ["v2"]]), "{label}");
        for (tool, sql, code) in [
            ("query", "INSERT INTO t VALUES (3)", "read_only"),
            ("query", "CREATE TABLE u (a INT)", "read_only"),
            ("query", "BEGIN", "transaction_control"),
            ("query", "SELECT SLEEP(5)", "timeout"),
            ("execute", "SELECT 1", "not_a_write"),
            ("execute", "COMMIT", "transaction_control"),
            ("execute", "DELETE FROM t", "approval_required"),
            (
                "execute",
                "UPDATE t SET a = 1 WHERE a = 2",
                "approval_required",
            ),
        ] {
            let e = server.err(tool, q(sql)).await;
            assert_eq!(e["code"], code, "{label}: {tool} {sql}: {e}");
        }
        let mut filed: Vec<(String, Vec<Unguarded>)> = server
            .approvals()
            .pending()
            .iter()
            .map(|r| (r.sql().to_owned(), r.unguarded().to_vec()))
            .collect();
        filed.sort_by(|a, b| a.0.cmp(&b.0));
        let want = vec![
            (
                "DELETE FROM t".to_owned(),
                vec![Unguarded::DeleteWithoutWhere],
            ),
            ("UPDATE t SET a = 1 WHERE a = 2".to_owned(), vec![]),
        ];
        assert_eq!(filed, want, "{label}");
        assert_eq!(
            db.scalar("SELECT COUNT(*) FROM t").await.as_deref(),
            Some("2"),
            "{label}"
        );
    }
}

#[tokio::test]
async fn a_client_cancel_stops_the_statement_on_the_server() {
    for (label, db) in my_matrix().await {
        let server = TestServer::mysql(&db, "").await;
        let sleeping = format!(
            "SELECT COUNT(*) FROM information_schema.PROCESSLIST WHERE INFO = 'SELECT SLEEP(60)' AND DB = '{}'",
            db.database()
        );
        let handle = server.start_call("query", q("SELECT SLEEP(60)")).await;
        assert!(
            until(&db, &sleeping, "1").await,
            "{label}: the sleep never started"
        );
        handle.cancel(None).await.expect("cancel sent");
        assert!(
            until(&db, &sleeping, "0").await,
            "{label}: the sleep runs on after the cancel"
        );
    }
}

#[tokio::test]
async fn the_catalog_tools_read_mysql_schemas_tables_and_columns() {
    for (label, db) in my_matrix().await {
        db.scalar(
            "CREATE TABLE t (id INT NOT NULL, code VARCHAR(10) NOT NULL DEFAULT 'x', \
             n INT DEFAULT 0, PRIMARY KEY (code, id))",
        )
        .await;
        db.scalar("CREATE VIEW v AS SELECT id FROM t").await;
        let server = TestServer::mysql(&db, "").await;
        let listed = server.ok("list_schemas", json!({"connection": "my"})).await;
        let names: Vec<&str> = listed["schemas"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| s["name"].as_str().unwrap())
            .collect();
        let n = names.len();
        assert!(
            n > 4 && names.contains(&db.database()),
            "{label}: {names:?}"
        );
        assert_eq!(
            &names[n - 4..],
            ["information_schema", "mysql", "performance_schema", "sys"],
            "{label}"
        );
        assert_eq!(
            server
                .ok(
                    "list_tables",
                    json!({"connection": "my", "schema": db.database()})
                )
                .await,
            json!({"tables": [{"name": "t", "kind": "table"}, {"name": "v", "kind": "view"}], "truncated": false}),
            "{label}"
        );
        assert_eq!(
            server
                .ok(
                    "describe_table",
                    json!({"connection": "my", "schema": db.database(), "table": "t"})
                )
                .await,
            json!({"columns": [
                {"name": "id", "type": "int", "nullable": false, "primary_key": 2, "default": null},
                {"name": "code", "type": "varchar(10)", "nullable": false, "primary_key": 1, "default": "'x'"},
                {"name": "n", "type": "int", "nullable": true, "primary_key": null, "default": "0"}
            ]}),
            "{label}"
        );
    }
}

#[tokio::test]
async fn a_mysql_error_carries_its_sqlstate_and_error_number() {
    for (label, db) in my_matrix().await {
        let server = TestServer::mysql(&db, "").await;
        let e = server.err("query", q("SELECT * FROM missing_t")).await;
        assert_eq!(
            (
                e["code"].clone(),
                e["sqlstate"].clone(),
                e["error_number"].clone()
            ),
            (json!("database"), json!("42S02"), json!("1146")),
            "{label}: {e}"
        );
        assert!(
            e["message"].as_str().unwrap().contains("missing_t"),
            "{label}: {e}"
        );
        let e = server.err("query", q("SELEC 1")).await;
        assert_eq!(
            (e["sqlstate"].clone(), e["error_number"].clone()),
            (json!("42000"), json!("1064")),
            "{label}: {e}"
        );
    }
}

/// A MySQL connection that is never dialled: `list_connections` and `explain`'s refusal run
/// before any session opens.
const UNDIALLED: &str = "dbento = 1\n[connections.my]\nengine = \"mysql\"\nhost = \"127.0.0.1\"\nport = 1\nuser = \"u\"\nsecret = \"none\"\n";

#[tokio::test]
async fn explain_on_mysql_is_unsupported_and_names_the_engine() {
    let server = TestServer::start(UNDIALLED).await;
    let listed = server.ok("list_connections", json!({})).await;
    assert_eq!(listed["connections"][0]["engine"], "mysql", "{listed}");
    let e = server.err("explain", q("SELECT 1")).await;
    assert_eq!(e["code"], "unsupported", "{e}");
    assert_eq!(
        e["message"], "`explain` is not available on mysql connections yet",
        "{e}"
    );
}

#[tokio::test]
async fn the_tool_descriptions_name_mysql() {
    let server = TestServer::start(UNDIALLED).await;
    let tools = server.tools().await;
    let description = |name: &str| {
        tools
            .iter()
            .find(|t| t.name == name)
            .and_then(|t| t.description.as_deref())
            .unwrap_or_default()
            .to_owned()
    };
    let listed = description("list_connections");
    assert!(
        listed.contains("sqlite, postgres, mysql or duckdb"),
        "{listed}"
    );
    let schemas = description("list_schemas");
    assert!(
        schemas.contains("On MySQL each database is a schema"),
        "{schemas}"
    );
    let explain = description("explain");
    assert!(explain.contains("not available on MySQL"), "{explain}");
}
