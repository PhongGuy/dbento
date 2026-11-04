//! `TestServer`: a `DbentoServer` on a temporary workspace, driven by an in-process rmcp
//! client over a duplex pipe, so tests see exactly what an AI client sees.
#![allow(dead_code)] // each test binary uses a different subset

use std::path::{Path, PathBuf};
use std::sync::Arc;

use dbento_core::MemoryStore;
use dbento_mcp::{DbentoServer, McpOptions};
use dbento_testkit::{MS_SECRET_VAR, MY_SECRET_VAR, MsDb, MyDb, PgDb, SECRET_VAR};
use rmcp::model::{CallToolRequest, CallToolRequestParams, CallToolResult, ClientRequest, Tool};
use rmcp::service::{PeerRequestOptions, RequestHandle, RunningService};
use rmcp::{RoleClient, ServiceExt};
use serde_json::Value;
use tempfile::TempDir;

/// A server on a temporary workspace and a connected client.
pub struct TestServer {
    dir: TempDir,
    /// Holds the history file, outside the workspace as in production.
    state: TempDir,
    client: RunningService<RoleClient, ()>,
}

impl TestServer {
    /// Serve a workspace whose `dbento.toml` is `toml`.
    pub async fn start(toml: &str) -> TestServer {
        TestServer::start_with(toml, |o| o).await
    }

    /// As `start`, with the options adjusted by `configure`.
    pub async fn start_with(
        toml: &str,
        configure: impl FnOnce(McpOptions) -> McpOptions,
    ) -> TestServer {
        let dir = TempDir::new().expect("tempdir");
        std::fs::write(dir.path().join("dbento.toml"), toml).expect("write");
        TestServer::serve(dir, configure).await
    }

    /// Serve a workspace whose connection `lite` is `data/app.db`, created by running `setup`;
    /// `extra_toml` is appended after the connection's table.
    pub async fn sqlite(setup: &str, extra_toml: &str) -> TestServer {
        TestServer::sqlite_with(setup, extra_toml, |o| o).await
    }

    /// As `sqlite`, with the options adjusted by `configure`.
    pub async fn sqlite_with(
        setup: &str,
        extra_toml: &str,
        configure: impl FnOnce(McpOptions) -> McpOptions,
    ) -> TestServer {
        let dir = TempDir::new().expect("tempdir");
        std::fs::create_dir(dir.path().join("data")).expect("mkdir");
        rusqlite::Connection::open(dir.path().join("data/app.db"))
            .expect("open")
            .execute_batch(setup)
            .expect("setup");
        let toml = format!(
            "dbento = 1\n[connections.lite]\nengine = \"sqlite\"\npath = \"data/app.db\"\n{extra_toml}\n"
        );
        std::fs::write(dir.path().join("dbento.toml"), toml).expect("write");
        TestServer::serve(dir, configure).await
    }

    /// Serve a workspace whose connection `pg` is `db`, its secret answered by the env lookup.
    pub async fn postgres(db: &PgDb, extra_toml: &str) -> TestServer {
        TestServer::postgres_with(db, extra_toml, |o| o).await
    }

    /// As `postgres`, with the options adjusted by `configure`.
    pub async fn postgres_with(
        db: &PgDb,
        extra_toml: &str,
        configure: impl FnOnce(McpOptions) -> McpOptions,
    ) -> TestServer {
        let password = db.password().to_owned();
        TestServer::postgres_secret(db, extra_toml, Some(password), configure).await
    }

    /// As `postgres_with`, with the secret variable answered by `secret`, or unset for `None`.
    pub async fn postgres_secret(
        db: &PgDb,
        extra_toml: &str,
        secret: Option<String>,
        configure: impl FnOnce(McpOptions) -> McpOptions,
    ) -> TestServer {
        let toml = format!("dbento = 1\n{}", db.connection_toml("pg", extra_toml));
        TestServer::start_with(&toml, move |o| {
            configure(o.env_lookup(move |v| {
                if v == SECRET_VAR {
                    secret.clone()
                } else {
                    None
                }
            }))
        })
        .await
    }

    /// Serve a workspace whose connection `my` is `db`, its secret answered by the env lookup.
    pub async fn mysql(db: &MyDb, extra_toml: &str) -> TestServer {
        let toml = format!("dbento = 1\n{}", db.connection_toml("my", extra_toml));
        let password = db.password().to_owned();
        TestServer::start_with(&toml, move |o| {
            o.env_lookup(move |v| (v == MY_SECRET_VAR).then(|| password.clone()))
        })
        .await
    }

    /// Serve a workspace whose connection `ms` is `db`, its secret answered by the env lookup.
    pub async fn mssql(db: &MsDb, extra_toml: &str) -> TestServer {
        let toml = format!("dbento = 1\n{}", db.connection_toml("ms", extra_toml));
        let password = db.password().to_owned();
        TestServer::start_with(&toml, move |o| {
            o.env_lookup(move |v| (v == MS_SECRET_VAR).then(|| password.clone()))
        })
        .await
    }

    async fn serve(dir: TempDir, configure: impl FnOnce(McpOptions) -> McpOptions) -> TestServer {
        let state = TempDir::new().expect("tempdir");
        let options = configure(
            McpOptions::new(dir.path())
                .history_file(state.path().join("history.jsonl"))
                .approvals_dir(state.path().join("approvals")),
        );
        // Secrets come from memory, never the machine's keychain.
        let server =
            DbentoServer::new(options, Arc::new(MemoryStore::new())).expect("server opens");
        // The server's handshake waits for the client's, so it runs on its own task.
        let (server_io, client_io) = tokio::io::duplex(1 << 20);
        tokio::spawn(async move {
            if let Ok(running) = server.serve(server_io).await {
                let _ = running.waiting().await;
            }
        });
        let client = ().serve(client_io).await.expect("client connects");
        TestServer { dir, state, client }
    }

    /// The workspace folder.
    pub fn root(&self) -> &Path {
        self.dir.path()
    }

    /// The history file the server appends to.
    pub fn history_file(&self) -> PathBuf {
        self.state.path().join("history.jsonl")
    }

    /// The approvals folder the server files requests in.
    pub fn approvals_dir(&self) -> PathBuf {
        self.state.path().join("approvals")
    }

    /// The approval store as the app would open it.
    pub fn approvals(&self) -> dbento_core::ApprovalStore {
        dbento_core::ApprovalStore::open(&self.approvals_dir()).expect("approvals folder")
    }

    /// Calls `execute`, requires `approval_required`, answers it with `decision` as the app
    /// would, and returns the request id.
    pub async fn decided(
        &self,
        connection: &str,
        sql: &str,
        decision: dbento_core::Decision,
    ) -> String {
        let e = self
            .err(
                "execute",
                serde_json::json!({"connection": connection, "sql": sql}),
            )
            .await;
        assert_eq!(e["code"], "approval_required", "{e}");
        let id = e["request_id"].as_str().expect("request_id").to_owned();
        self.approvals().decide(&id, decision).expect("decide");
        id
    }

    /// A scalar from the SQLite connection's `data/app.db`.
    pub fn count(&self, sql: &str) -> i64 {
        rusqlite::Connection::open(self.root().join("data/app.db"))
            .expect("open")
            .query_row(sql, [], |r| r.get(0))
            .expect("count")
    }

    /// The server's `instructions` from the handshake.
    pub fn instructions(&self) -> Option<String> {
        self.client
            .peer_info()
            .and_then(|info| info.instructions.clone())
    }

    /// The tools the server lists.
    pub async fn tools(&self) -> Vec<Tool> {
        self.client.list_all_tools().await.expect("tools/list")
    }

    /// Call `tool` with `args`, a JSON object; panics on a JSON-RPC error.
    pub async fn call(&self, tool: &str, args: Value) -> CallToolResult {
        let Value::Object(args) = args else {
            panic!("tool arguments must be a JSON object")
        };
        self.client
            .call_tool(CallToolRequestParams::new(tool.to_owned()).with_arguments(args))
            .await
            .unwrap_or_else(|e| panic!("{tool}: {e}"))
    }

    /// Call `tool`, require success, and return its structured content.
    pub async fn ok(&self, tool: &str, args: Value) -> Value {
        let result = self.call(tool, args).await;
        assert_ne!(
            result.is_error,
            Some(true),
            "{tool} failed: {}",
            text(&result)
        );
        result.structured_content.expect("structured content")
    }

    /// Call `tool`, require an `isError` result, and return its `error` object.
    pub async fn err(&self, tool: &str, args: Value) -> Value {
        let result = self.call(tool, args).await;
        assert_eq!(
            result.is_error,
            Some(true),
            "{tool} succeeded: {}",
            text(&result)
        );
        result.structured_content.expect("structured content")["error"].clone()
    }

    /// Start a call to `tool` without waiting for its result, so a test can cancel it.
    pub async fn start_call(&self, tool: &str, args: Value) -> RequestHandle<RoleClient> {
        let Value::Object(args) = args else {
            panic!("tool arguments must be a JSON object")
        };
        let params = CallToolRequestParams::new(tool.to_owned()).with_arguments(args);
        self.client
            .send_request_with_option(
                ClientRequest::CallToolRequest(CallToolRequest::new(params)),
                PeerRequestOptions::no_options(),
            )
            .await
            .expect("request sent")
    }
}

/// A result's text content, joined by newlines.
pub fn text(result: &CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|c| c.as_text().map(|t| t.text.clone()))
        .collect::<Vec<_>>()
        .join("\n")
}

/// The nonce and the JSON inside a fenced text block; panics when the block is not fenced.
pub fn unfence(text: &str) -> (String, Value) {
    let mut lines = text.lines();
    let warning = lines.next().expect("a warning line");
    assert!(warning.contains("untrusted"), "{warning}");
    let open = lines.next().expect("an opening tag");
    let nonce = open
        .strip_prefix("<untrusted-data-")
        .and_then(|s| s.strip_suffix('>'))
        .unwrap_or_else(|| panic!("not an opening tag: {open}"))
        .to_owned();
    let rest: Vec<&str> = lines.collect();
    let (close, body) = rest.split_last().expect("a closing tag");
    assert_eq!(*close, format!("</untrusted-data-{nonce}>"));
    (
        nonce,
        serde_json::from_str(&body.join("\n")).expect("JSON inside the fence"),
    )
}
