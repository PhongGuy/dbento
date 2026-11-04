//! MCP server for Dbento: serves a workspace's databases to an AI client, read-only except
//! for `execute`, which runs one write only after the user approves that exact call in the app.
//! Every tool calls `dbento-core`, so the client sees what the CLI and app see.

#![forbid(unsafe_code)]
#![deny(missing_docs)]

mod error;
mod execute;
mod explain;
mod fence;
mod query;
mod saved;
mod server;
mod session;

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use rmcp::ServiceExt;

pub use server::DbentoServer;

/// Looks up an environment variable by name; tests answer it without touching the process.
type EnvLookup = Arc<dyn Fn(&str) -> Option<String> + Send + Sync>;

/// The workspace to serve and the bounds every tool call runs under.
#[derive(Clone)]
pub struct McpOptions {
    workspace: PathBuf,
    expose: Option<BTreeSet<String>>,
    timeout: Duration,
    history_file: Option<PathBuf>,
    approvals_dir: Option<PathBuf>,
    env_lookup: EnvLookup,
}

impl McpOptions {
    /// Serve every connection of the workspace at `workspace`, with a 30 second statement
    /// timeout, no history, and secrets' environment variables read from the process.
    pub fn new(workspace: impl Into<PathBuf>) -> Self {
        McpOptions {
            workspace: workspace.into(),
            expose: None,
            timeout: Duration::from_secs(30),
            history_file: None,
            approvals_dir: None,
            env_lookup: Arc::new(|name| std::env::var(name).ok()),
        }
    }

    /// Expose only the connections named in `names`; the rest look nonexistent.
    pub fn expose(mut self, names: impl IntoIterator<Item = impl Into<String>>) -> Self {
        self.expose = Some(names.into_iter().map(Into::into).collect());
        self
    }

    /// The statement timeout for every query.
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Append every query to the history file at `path`.
    pub fn history_file(mut self, path: impl Into<PathBuf>) -> Self {
        self.history_file = Some(path.into());
        self
    }

    /// File approval requests and claim grants under `dir`; without it `execute` answers
    /// `approvals_unavailable`.
    pub fn approvals_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.approvals_dir = Some(dir.into());
        self
    }

    /// Answer secrets' environment variables with `lookup` instead of the process environment.
    pub fn env_lookup(
        mut self,
        lookup: impl Fn(&str) -> Option<String> + Send + Sync + 'static,
    ) -> Self {
        self.env_lookup = Arc::new(lookup);
        self
    }
}

/// Why the server could not open its workspace or keep its transport running.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum McpError {
    /// The workspace failed to open.
    #[error(transparent)]
    Open(#[from] dbento_core::OpenError),
    /// The stdio transport failed to start or broke while serving.
    #[error("MCP transport: {0}")]
    Transport(String),
}

/// Serves `server` over stdin and stdout until the client disconnects.
pub async fn serve_stdio(server: DbentoServer) -> Result<(), McpError> {
    let running = server
        .serve(rmcp::transport::stdio())
        .await
        .map_err(|e| McpError::Transport(e.to_string()))?;
    running
        .waiting()
        .await
        .map_err(|e| McpError::Transport(e.to_string()))?;
    Ok(())
}
