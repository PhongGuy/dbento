//! `dbento mcp`: the real binary over stdio, driven by an rmcp client as an AI host would.
use std::path::Path;
use std::time::Duration;

use crate::common::{LITE_TOML, dbento, sqlite_ws};
use rmcp::ServiceExt;
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::transport::TokioChildProcess;
use serde_json::{Value, json};

/// Spawn `dbento <args>` in `cwd` with `DBENTO_STATE_DIR=state`, make each call in order and
/// return the results. Bounded to 30 s; the child is killed when dropped.
async fn calls(
    cwd: &Path,
    state: &Path,
    args: &[&str],
    calls: &[(&str, Value)],
) -> Vec<CallToolResult> {
    let mut cmd = tokio::process::Command::new(env!("CARGO_BIN_EXE_dbento"));
    cmd.args(args)
        .current_dir(cwd)
        .env("DBENTO_STATE_DIR", state)
        .kill_on_drop(true);
    let exchange = async {
        let client =
            ().serve(TokioChildProcess::new(cmd).expect("spawns"))
                .await
                .expect("handshake");
        let mut out = Vec::new();
        for (tool, args) in calls {
            let Value::Object(args) = args.clone() else {
                panic!("tool arguments must be a JSON object")
            };
            let params = CallToolRequestParams::new(tool.to_string()).with_arguments(args);
            out.push(client.call_tool(params).await.expect("tools/call"));
        }
        client.cancel().await.expect("closes");
        out
    };
    tokio::time::timeout(Duration::from_secs(30), exchange)
        .await
        .expect("the stdio exchange finishes within 30 s")
}

fn structured(r: &CallToolResult) -> &Value {
    r.structured_content.as_ref().expect("structured content")
}

#[tokio::test]
async fn serves_a_query_over_stdio_and_records_it_in_the_state_dir() {
    let ws = sqlite_ws("CREATE TABLE t(a INTEGER); INSERT INTO t VALUES (1), (2);");
    let state = tempfile::TempDir::new().unwrap();
    let r = calls(
        ws.path(),
        state.path(),
        &["mcp"],
        &[
            ("list_connections", json!({})),
            (
                "query",
                json!({"connection": "lite", "sql": "SELECT a FROM t ORDER BY a"}),
            ),
        ],
    )
    .await;
    assert_eq!(structured(&r[0])["connections"][0]["name"], "lite");
    assert_eq!(structured(&r[1])["rows"], json!([["1"], ["2"]]));
    let history =
        std::fs::read_to_string(state.path().join("history.jsonl")).expect("history written");
    assert_eq!(history.lines().count(), 1, "{history}");
    assert!(history.contains("\"source\":\"mcp\""), "{history}");
}

#[tokio::test]
async fn execute_files_its_request_in_the_state_dirs_approvals_folder() {
    let ws = sqlite_ws("CREATE TABLE t(a INTEGER);");
    let state = tempfile::TempDir::new().unwrap();
    let r = calls(
        ws.path(),
        state.path(),
        &["mcp"],
        &[(
            "execute",
            json!({"connection": "lite", "sql": "INSERT INTO t VALUES (1)"}),
        )],
    )
    .await;
    let e = &structured(&r[0])["error"];
    assert_eq!(e["code"], "approval_required", "{e}");
    let id = e["request_id"].as_str().expect("request_id");
    assert!(
        state
            .path()
            .join("approvals/inbox")
            .join(format!("{id}.json"))
            .is_file(),
        "no request under the state dir"
    );
}

#[tokio::test]
async fn workspace_and_connection_flags_restrict_what_is_served() {
    let ws = sqlite_ws("CREATE TABLE t(a INTEGER);");
    std::fs::write(
        ws.path().join("dbento.toml"),
        format!("{LITE_TOML}[connections.other]\nengine = \"sqlite\"\npath = \"data/app.db\"\n"),
    )
    .unwrap();
    let elsewhere = tempfile::TempDir::new().unwrap();
    let state = tempfile::TempDir::new().unwrap();
    let dir = ws.path().to_str().unwrap();
    let r = calls(
        elsewhere.path(),
        state.path(),
        &[
            "mcp",
            "--workspace",
            dir,
            "--connection",
            "lite",
            "--timeout",
            "5",
        ],
        &[
            ("list_connections", json!({})),
            ("list_schemas", json!({"connection": "other"})),
        ],
    )
    .await;
    let names: Vec<&Value> = structured(&r[0])["connections"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| &c["name"])
        .collect();
    assert_eq!(names, vec![&json!("lite")]);
    assert_eq!(r[1].is_error, Some(true));
    assert_eq!(structured(&r[1])["error"]["code"], "no_connection");
}

#[test]
fn a_client_disconnect_after_the_handshake_exits_0() {
    let ws = sqlite_ws("CREATE TABLE t(a INTEGER);");
    let state = tempfile::TempDir::new().unwrap();
    // An initialize, its acknowledgement, then end of input: the client hung up.
    let handshake = concat!(
        r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-06-18","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#,
        "\n",
        r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#,
        "\n"
    );
    dbento()
        .arg("mcp")
        .current_dir(ws.path())
        .env("DBENTO_STATE_DIR", state.path())
        .write_stdin(handshake)
        .timeout(Duration::from_secs(30))
        .assert()
        .code(0);
}

#[test]
fn no_workspace_exits_2_with_a_message_on_stderr() {
    let empty = tempfile::TempDir::new().unwrap();
    let assert = dbento()
        .arg("mcp")
        .current_dir(empty.path())
        .timeout(Duration::from_secs(30))
        .assert()
        .code(2)
        .stdout("");
    let stderr = String::from_utf8_lossy(&assert.get_output().stderr).into_owned();
    assert!(stderr.contains("no dbento.toml"), "{stderr}");
}

#[test]
fn mcp_help_lists_its_flags() {
    let out = dbento().args(["mcp", "--help"]).output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    let help = String::from_utf8_lossy(&out.stdout);
    assert!(
        help.contains("--connection") && help.contains("--timeout"),
        "{help}"
    );
}
