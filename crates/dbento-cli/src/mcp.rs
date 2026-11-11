//! `dbento mcp`: open the workspace as the other subcommands do and serve it over stdio.
//! Stdout carries the protocol, so every message here goes to stderr.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use dbento_mcp::{DbentoServer, McpOptions};

use crate::{Exit, open_workspace, secret};

#[derive(clap::Args)]
pub(crate) struct McpArgs {
    /// Expose only this connection; repeat for several. A parent with `databases` also exposes
    /// every database it defines: adding `databases = "all"` widens this to every database its
    /// login can reach, and their results are untrusted input. Without it every connection is
    /// served.
    #[arg(long = "connection", value_name = "NAME")]
    pub connections: Vec<String>,
    /// The statement timeout for every call, in seconds; a connection's shorter timeout wins.
    #[arg(long, value_name = "SECS", default_value_t = 30, value_parser = clap::value_parser!(u64).range(1..))]
    pub timeout: u64,
}

/// Exit 0 when the client disconnects, 2 when no workspace opens (message on stderr), 1 on a
/// transport failure. Nothing but the protocol is written to stdout.
pub(crate) async fn serve(workspace: Option<&Path>, args: &McpArgs) -> anyhow::Result<Exit> {
    let server = match server(workspace, args) {
        Ok(server) => server,
        Err(e) => {
            eprintln!("error: {e:#}");
            return Ok(Exit::Refused);
        }
    };
    match dbento_mcp::serve_stdio(server).await {
        Ok(()) => Ok(Exit::Ok),
        Err(e) => {
            eprintln!("error: {e}");
            Ok(Exit::Failed)
        }
    }
}

/// The server for the resolved workspace, with history and approvals shared with the app's
/// state directory.
fn server(workspace: Option<&Path>, args: &McpArgs) -> anyhow::Result<DbentoServer> {
    let ws = open_workspace(workspace)?;
    let mut options = McpOptions::new(ws.dir()).timeout(Duration::from_secs(args.timeout));
    // Without the flag nothing is filtered; an empty expose set would hide every connection.
    if !args.connections.is_empty() {
        options = options.expose(args.connections.iter().cloned());
    }
    if let Some(path) = dbento_core::paths::history_file() {
        options = options.history_file(path);
    }
    if let Some(path) = dbento_core::paths::approvals_dir() {
        options = options.approvals_dir(path);
    }
    Ok(DbentoServer::new(options, Arc::new(secret::store()))?)
}
