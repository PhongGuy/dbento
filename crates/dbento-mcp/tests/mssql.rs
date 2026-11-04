//! The MCP tools on SQL Server: what the read-only fence finds reaches the client as an error,
//! writes and transaction control are refused, writes are filed with their unguarded kinds, and
//! the timeout and a client cancel stop the batch on the server.
mod common;

use std::time::Duration;

use common::TestServer;
use dbento_core::{Decision, Unguarded};
use dbento_testkit::{MsDb, mssql_db};
use serde_json::{Value, json};

const CUBE: &str = "SELECT a.object_id FROM sys.all_objects a CROSS JOIN sys.all_objects b \
                    CROSS JOIN sys.all_objects c";

fn q(sql: &str) -> Value {
    json!({"connection": "ms", "sql": sql, "max_rows": 2})
}

async fn with_table() -> (MsDb, TestServer) {
    let db = mssql_db().await;
    db.scalar("CREATE TABLE dbo.t (id int IDENTITY PRIMARY KEY, v nvarchar(20))")
        .await;
    let server = TestServer::mssql(&db, "tls = \"off\"").await;
    (db, server)
}

#[tokio::test]
async fn the_query_tool_reports_a_hidden_commit_it_finds_on_closing() {
    dbento_testkit::skip_without_mssql!();
    let (db, server) = with_table().await;
    let e = server
        .err(
            "query",
            q("INSERT dbo.t (v) VALUES (N'm1') EXEC('COMMIT COMMIT') SELECT 1 AS one"),
        )
        .await;
    assert_eq!(e["code"], "read_only", "{e}");
    assert!(
        e["message"]
            .as_str()
            .unwrap()
            .contains("may have committed"),
        "{e}"
    );
    let kept = db
        .scalar("SELECT COUNT(*) FROM dbo.t WHERE v = N'm1'")
        .await;
    assert_eq!(kept.as_deref(), Some("1"));
}

#[tokio::test]
async fn the_query_tool_reports_a_hidden_commit_before_a_cut() {
    dbento_testkit::skip_without_mssql!();
    let (_db, server) = with_table().await;
    let sql =
        format!("SET NOCOUNT ON EXEC('INSERT dbo.t (v) VALUES (N''m2'') COMMIT COMMIT') {CUBE}");
    let e = server.err("query", q(&sql)).await;
    assert_eq!(e["code"], "read_only", "{e}");
}

#[tokio::test]
async fn the_query_tool_cuts_a_plain_select_without_a_report() {
    dbento_testkit::skip_without_mssql!();
    let (_db, server) = with_table().await;
    let ok = server.ok("query", q(CUBE)).await;
    assert_eq!(ok["truncated"], true, "{ok}");
}

// A later item with rows is cut at once and drained under the cap, so a slow one stops the batch.
// The batch is stopped, so no verdict exists, but the row is kept and success would be a lie.
#[tokio::test]
async fn the_query_tool_fails_when_closing_stopped_the_batch_at_the_drain_cap() {
    dbento_testkit::skip_without_mssql!();
    let (db, server) = with_table().await;
    let sql =
        format!("SELECT 1 AS one EXEC('INSERT dbo.t (v) VALUES (N''m4'') COMMIT COMMIT') {CUBE}");
    let e = server.err("query", q(&sql)).await;
    assert_eq!(e["code"], "session_closed", "{e}");
    assert!(
        e["message"]
            .as_str()
            .unwrap()
            .contains("keep what they wrote"),
        "{e}"
    );
    let kept = db
        .scalar("SELECT COUNT(*) FROM dbo.t WHERE v = N'm4'")
        .await;
    assert_eq!(kept.as_deref(), Some("1"));
}

// The tool reads the batch to its end, so the commit behind the first result is judged.
#[tokio::test]
async fn the_query_tool_reports_a_commit_behind_a_slow_rest_of_the_batch() {
    dbento_testkit::skip_without_mssql!();
    let (db, server) = with_table().await;
    let sql = "SELECT 1 AS one EXEC('INSERT dbo.t (v) VALUES (N''m3'') COMMIT COMMIT') \
               RAISERROR('f', 0, 1) WITH NOWAIT WAITFOR DELAY '00:00:02'";
    let e = server.err("query", q(sql)).await;
    assert_eq!(e["code"], "read_only", "{e}");
    let kept = db
        .scalar("SELECT COUNT(*) FROM dbo.t WHERE v = N'm3'")
        .await;
    assert_eq!(kept.as_deref(), Some("1"));
}

#[tokio::test]
async fn the_query_tool_returns_the_first_result_and_counts_the_rest_of_the_batch() {
    dbento_testkit::skip_without_mssql!();
    let (_db, server) = with_table().await;
    let ok = server
        .ok("query", q("SELECT 1 AS a SELECT 2 AS b SELECT 3 AS c"))
        .await;
    assert_eq!(ok["rows"], json!([["1"]]), "{ok}");
    assert_eq!(ok["extra_results"], 2, "{ok}");
    let ok = server.ok("query", q("SELECT 1 AS a")).await;
    assert_eq!(ok["extra_results"], 0, "{ok}");
}

// A cut sends Attention, so the rest of the batch never runs and there is nothing to count.
#[tokio::test]
async fn a_cut_first_result_stops_the_batch_and_counts_nothing() {
    dbento_testkit::skip_without_mssql!();
    let (db, server) = with_table().await;
    let sql = "SET NOCOUNT ON SELECT a.object_id FROM sys.all_objects a \
               CROSS JOIN sys.all_objects b SELECT 9 AS b";
    let ok = server.ok("query", q(sql)).await;
    assert_eq!(ok["rows"].as_array().unwrap().len(), 2, "{ok}");
    assert_eq!(
        (&ok["truncated"], &ok["extra_results"]),
        (&json!(true), &json!(0)),
        "{ok}"
    );
    assert_eq!(db.scalar("SELECT 1").await.as_deref(), Some("1"));
}

#[tokio::test]
async fn a_single_query_cut_stops_with_no_count() {
    dbento_testkit::skip_without_mssql!();
    let (_db, server) = with_table().await;
    let ok = server.ok("query", q(CUBE)).await;
    assert_eq!(
        (&ok["truncated"], &ok["extra_results"]),
        (&json!(true), &json!(0)),
        "{ok}"
    );
}

#[tokio::test]
async fn a_go_count_runs_each_repeat_and_counts_them() {
    dbento_testkit::skip_without_mssql!();
    let (_db, server) = with_table().await;
    let ok = server
        .ok("query", q("SELECT COUNT(*) AS n FROM dbo.t\nGO 3"))
        .await;
    assert_eq!(ok["rows"], json!([["0"]]), "{ok}");
    assert_eq!(ok["extra_results"], 2, "{ok}");
}

#[tokio::test]
async fn the_query_tool_fails_when_the_rest_of_the_batch_failed() {
    dbento_testkit::skip_without_mssql!();
    let (_db, server) = with_table().await;
    let e = server
        .err("query", q("SELECT 1 AS a SELECT 1/0 AS b"))
        .await;
    assert_eq!(e["code"], "database", "{e}");
    assert_eq!(e["error_number"], "8134", "{e}");
    assert_eq!(
        (e["class"].as_u64(), e["state"].as_u64()),
        (Some(16), Some(1)),
        "{e}"
    );
    assert!(e.get("sqlstate").is_none(), "{e}");
}

// With a reader principal the database refuses the writes, so these pin the same tools on it.
const READER_SETUP: &str = "CREATE USER dbento_reader WITHOUT LOGIN; \
    ALTER ROLE db_datareader ADD MEMBER dbento_reader; \
    ALTER ROLE db_denydatawriter ADD MEMBER dbento_reader; \
    GRANT VIEW DEFINITION TO dbento_reader; \
    DENY ALTER ANY USER TO dbento_reader";

async fn with_reader() -> (MsDb, TestServer) {
    let db = mssql_db().await;
    db.scalar("CREATE TABLE dbo.t (id int IDENTITY PRIMARY KEY, v nvarchar(20))")
        .await;
    db.scalar(READER_SETUP).await;
    let extra = "tls = \"off\"\nread_only_user = \"dbento_reader\"";
    let server = TestServer::mssql(&db, extra).await;
    (db, server)
}

#[tokio::test]
async fn a_refused_write_behind_the_first_result_fails_the_call_on_a_reader() {
    dbento_testkit::skip_without_mssql!();
    let (db, server) = with_reader().await;
    let e = server
        .err("query", q("SELECT 1 AS a INSERT dbo.t (v) VALUES (N'r1')"))
        .await;
    assert_eq!(e["code"], "read_only", "{e}");
    let kept = db
        .scalar("SELECT COUNT(*) FROM dbo.t WHERE v = N'r1'")
        .await;
    assert_eq!(kept.as_deref(), Some("0"));
}

#[tokio::test]
async fn the_query_tool_runs_as_the_reader_and_the_database_refuses_its_writes() {
    dbento_testkit::skip_without_mssql!();
    let (db, server) = with_reader().await;
    let who = server.ok("query", q("SELECT USER_NAME() AS u")).await;
    assert_eq!(who["rows"][0][0], "dbento_reader", "{who}");
    let e = server
        .err("query", q("INSERT dbo.t (v) VALUES (N'r1')"))
        .await;
    assert_eq!(e["code"], "read_only", "{e}");
    assert!(
        e["message"].as_str().unwrap().contains("permission"),
        "the server's own words: {e}"
    );
    // A refusal behind the first result still fails the call.
    let e = server
        .err("query", q("SELECT 1 AS a INSERT dbo.t (v) VALUES (N'r2')"))
        .await;
    assert_eq!(e["code"], "read_only", "{e}");
    assert_eq!(
        db.scalar("SELECT COUNT(*) FROM dbo.t").await.as_deref(),
        Some("0")
    );
}

// These two fail under best effort, where a later error leaves the writes unknown. Here the
// database refused every write, so the first result stands, as on every other engine.
#[tokio::test]
async fn a_database_enforced_query_keeps_its_result_when_the_rest_of_the_batch_failed() {
    dbento_testkit::skip_without_mssql!();
    let (_db, server) = with_reader().await;
    let ok = server.ok("query", q("SELECT 1 AS a SELECT 1/0 AS b")).await;
    assert_eq!(ok["rows"][0][0], "1", "{ok}");
}

#[tokio::test]
async fn a_database_enforced_query_keeps_its_result_past_the_drain_cap() {
    dbento_testkit::skip_without_mssql!();
    let (_db, server) = with_reader().await;
    let sql = "SELECT 1 AS one RAISERROR('f', 0, 1) WITH NOWAIT WAITFOR DELAY '00:00:20'";
    let ok = server.ok("query", q(sql)).await;
    assert_eq!(ok["rows"][0][0], "1", "{ok}");
}

#[tokio::test]
async fn the_execute_tool_still_files_a_write_on_a_reader_connection() {
    dbento_testkit::skip_without_mssql!();
    let (db, server) = with_reader().await;
    let e = server
        .err(
            "execute",
            json!({"connection": "ms", "sql": "INSERT dbo.t (v) VALUES (N'r3')"}),
        )
        .await;
    assert_eq!(e["code"], "approval_required", "{e}");
    assert_eq!(
        db.scalar("SELECT COUNT(*) FROM dbo.t").await.as_deref(),
        Some("0")
    );
}

fn plain(sql: &str) -> Value {
    json!({"connection": "ms", "sql": sql})
}

async fn until(db: &MsDb, sql: &str, want: &str) -> bool {
    for _ in 0..50 {
        if db.scalar(sql).await.as_deref() == Some(want) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

#[tokio::test]
async fn the_tools_refuse_writes_and_transaction_control_and_file_unguarded_writes() {
    dbento_testkit::skip_without_mssql!();
    let db = mssql_db().await;
    db.scalar("CREATE TABLE dbo.t (a int)").await;
    db.scalar("INSERT dbo.t VALUES (1), (2)").await;
    let server = TestServer::mssql(&db, "tls = \"off\"\nstatement_timeout = \"500ms\"").await;
    let r = server
        .ok(
            "query",
            plain("SELECT CONCAT(N'v', a) AS v FROM dbo.t ORDER BY a"),
        )
        .await;
    assert_eq!(r["rows"], json!([["v1"], ["v2"]]));
    for (tool, sql, code) in [
        ("query", "INSERT dbo.t VALUES (3)", "read_only"),
        ("query", "CREATE TABLE dbo.u (a int)", "read_only"),
        ("query", "BEGIN TRANSACTION", "transaction_control"),
        ("query", "WAITFOR DELAY '00:00:05'", "timeout"),
        ("execute", "SELECT 1", "not_a_write"),
        ("execute", "COMMIT", "transaction_control"),
        ("execute", "DELETE FROM dbo.t", "approval_required"),
        (
            "execute",
            "UPDATE dbo.t SET a = 1 WHERE a = 2",
            "approval_required",
        ),
    ] {
        let e = server.err(tool, plain(sql)).await;
        assert_eq!(e["code"], code, "{tool} {sql}: {e}");
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
            "DELETE FROM dbo.t".to_owned(),
            vec![Unguarded::DeleteWithoutWhere],
        ),
        ("UPDATE dbo.t SET a = 1 WHERE a = 2".to_owned(), vec![]),
    ];
    assert_eq!(filed, want);
    assert_eq!(
        db.scalar("SELECT COUNT(*) FROM dbo.t").await.as_deref(),
        Some("2")
    );
}

#[tokio::test]
async fn a_client_cancel_stops_the_batch_on_the_server() {
    dbento_testkit::skip_without_mssql!();
    let db = mssql_db().await;
    let server = TestServer::mssql(&db, "tls = \"off\"").await;
    let waiting = format!(
        "SELECT COUNT(*) FROM sys.dm_exec_requests r JOIN sys.dm_exec_sessions s \
         ON s.session_id = r.session_id WHERE r.wait_type = N'WAITFOR' AND s.database_id = DB_ID(N'{}')",
        db.database()
    );
    let handle = server
        .start_call("query", plain("WAITFOR DELAY '00:01:00'"))
        .await;
    assert!(until(&db, &waiting, "1").await, "the wait never started");
    handle.cancel(None).await.expect("cancel sent");
    assert!(
        until(&db, &waiting, "0").await,
        "the wait runs on after the cancel"
    );
}

async fn count(db: &MsDb, v: &str) -> Option<String> {
    let sql = format!("SELECT COUNT(*) FROM dbo.t WHERE v = N'{v}'");
    db.scalar(&sql).await
}

// The first item commits the row and the rest runs past the drain cap as the session closes.
#[tokio::test]
async fn the_execute_tool_does_not_claim_a_committed_write_was_rolled_back() {
    dbento_testkit::skip_without_mssql!();
    let (db, server) = with_table().await;
    let sql = "INSERT dbo.t (v) VALUES (N'x1') RAISERROR('f', 0, 1) WITH NOWAIT \
               WAITFOR DELAY '00:00:30'";
    server.decided("ms", sql, Decision::Approve).await;
    let e = server.err("execute", plain(sql)).await;
    assert_eq!(e["code"], "session_closed", "{e}");
    let message = e["message"].as_str().unwrap();
    assert!(message.contains("keep what they wrote"), "{e}");
    assert!(
        !message.contains("any writes it made were rolled back"),
        "{e}"
    );
    assert_eq!(count(&db, "x1").await.as_deref(), Some("1"));
}

#[tokio::test]
async fn the_tools_say_a_timed_out_statement_may_have_kept_its_writes() {
    dbento_testkit::skip_without_mssql!();
    let db = mssql_db().await;
    db.scalar("CREATE TABLE dbo.t (id int IDENTITY PRIMARY KEY, v nvarchar(20))")
        .await;
    let server = TestServer::mssql(&db, "tls = \"off\"\nstatement_timeout = \"1s\"").await;
    let sql = "UPDATE dbo.t SET v = N'x2' WHERE 1 = 0 WAITFOR DELAY '00:00:05'";
    let query = server.err("query", plain("WAITFOR DELAY '00:00:05'")).await;
    server.decided("ms", sql, Decision::Approve).await;
    let execute = server.err("execute", plain(sql)).await;
    for e in [query, execute] {
        assert_eq!(e["code"], "timeout", "{e}");
        assert!(
            e["message"].as_str().unwrap().contains("may have kept"),
            "{e}"
        );
    }
}

// The first result reads clean, so only closing can say the batch never finished.
#[tokio::test]
async fn the_execute_tool_fails_when_the_timeout_stopped_the_batch_behind_its_first_result() {
    dbento_testkit::skip_without_mssql!();
    let db = mssql_db().await;
    db.scalar("CREATE TABLE dbo.t (id int IDENTITY PRIMARY KEY, v nvarchar(20))")
        .await;
    let server = TestServer::mssql(&db, "tls = \"off\"\nstatement_timeout = \"1s\"").await;
    let sql = "INSERT dbo.t (v) VALUES (N'x3') RAISERROR('f', 0, 1) WITH NOWAIT \
               WAITFOR DELAY '00:00:05' INSERT dbo.t (v) VALUES (N'x4')";
    server.decided("ms", sql, Decision::Approve).await;
    let e = server.err("execute", plain(sql)).await;
    assert_eq!(e["code"], "timeout", "{e}");
    assert_eq!(count(&db, "x3").await.as_deref(), Some("1"));
    assert_eq!(count(&db, "x4").await.as_deref(), Some("0"));
}

// The forms the best-effort description quotes, on a server that can spare them. SHUTDOWN is
// classified only, in classify/mssql/tests.rs: it would stop the fixture's server.
#[tokio::test]
async fn the_hidden_commits_the_description_quotes_do_what_it_says() {
    dbento_testkit::skip_without_mssql!();
    let (db, server) = with_table().await;
    db.scalar("CREATE DATABASE dbento_p4_victim").await;
    let e = server
        .err(
            "query",
            q("SELECT 1 AS one EXEC('COMMIT COMMIT DROP DATABASE dbento_p4_victim')"),
        )
        .await;
    assert_eq!(e["code"], "read_only", "{e}");
    let gone = db.scalar("SELECT DB_ID(N'dbento_p4_victim')").await;
    assert_eq!(gone, None);
    let e = server
        .err("query", q("SELECT 1 AS one EXEC('COMMIT COMMIT KILL 999')"))
        .await;
    assert_eq!(e["error_number"], "6106", "{e}");
}

#[tokio::test]
async fn a_reader_refuses_the_hidden_commits_the_description_says_it_fixes() {
    dbento_testkit::skip_without_mssql!();
    let (db, server) = with_reader().await;
    db.scalar("CREATE DATABASE dbento_p4_kept").await;
    let e = server
        .err(
            "query",
            q("SELECT 1 AS one EXEC('COMMIT COMMIT DROP DATABASE dbento_p4_kept')"),
        )
        .await;
    assert_eq!(e["code"], "read_only", "{e}");
    let kept = db.scalar("SELECT DB_ID(N'dbento_p4_kept')").await;
    assert!(kept.is_some());
    db.scalar("DROP DATABASE dbento_p4_kept").await;
    let e = server
        .err("query", q("SELECT 1 AS one EXEC('COMMIT COMMIT KILL 1')"))
        .await;
    assert_eq!(e["code"], "read_only", "{e}");
}
