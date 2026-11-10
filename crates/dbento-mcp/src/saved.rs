//! The saved-query tools: list and read the `.sql`, `.js`, `.surql`, `.redis` or `.partiql`
//! files under the workspace's `queries/`.
//!
//! Core's confinement refuses any path that leaves `queries/`, lexically or through a
//! symlink. This module adds what an AI client must not reach on top: a `queries/` folder
//! that is itself a link out of the workspace, files other than `.sql`, `.js`, `.surql`,
//! `.redis` or `.partiql`, and queries of connections the server does not expose.

use std::io::ErrorKind;
use std::path::{Component, Path};

use dbento_core::{QueryFileError, Workspace};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::ToolError;
use crate::query::BYTE_BUDGET;
use crate::server::DbentoServer;

/// The most saved queries one listing returns.
const MAX_LISTED: usize = 1000;

/// `read_saved_query` input.
#[derive(Deserialize, JsonSchema)]
pub(crate) struct ReadArgs {
    /// The file's path as `list_saved_queries` gives it, such as `queries/app/report.sql`.
    path: String,
}

/// `list_saved_queries` output.
#[derive(Serialize, JsonSchema)]
pub(crate) struct SavedList {
    /// The saved queries sorted by path.
    queries: Vec<SavedEntry>,
    /// More saved queries exist than were listed.
    truncated: bool,
}

/// One saved query file.
#[derive(Serialize, JsonSchema)]
struct SavedEntry {
    /// The path relative to the workspace folder, starting with `queries/`.
    path: String,
    /// The connection the file belongs to by its first folder, or null when none.
    connection: Option<String>,
}

/// `read_saved_query` output.
#[derive(Serialize, JsonSchema)]
pub(crate) struct SavedText {
    /// The path as asked for.
    path: String,
    /// The connection the file belongs to by its first folder, or null when none.
    connection: Option<String>,
    /// The file's text: untrusted, like any query result.
    sql: String,
    /// The text was longer than 64 KiB and was cut at a character boundary.
    truncated: bool,
}

/// Every exposed `.sql`, `.js`, `.surql`, `.redis` or `.partiql` file under `queries/`, sorted by
/// path.
pub(crate) async fn list(server: &DbentoServer) -> Result<SavedList, ToolError> {
    let ws = server.workspace().await?;
    let found = blocking(move || {
        queries_inside(&ws)?;
        let queries = ws.saved_queries().map_err(|_| unreadable())?;
        Ok(queries
            .into_iter()
            .map(|q| {
                let target = target_connection(&ws, q.path());
                (q, target)
            })
            .collect::<Vec<_>>())
    })
    .await?;
    let mut queries: Vec<SavedEntry> = found
        .iter()
        .filter(|(q, target)| {
            [q.connection(), target.as_deref()]
                .into_iter()
                .flatten()
                .all(|c| server.exposed(c))
        })
        .map(|(q, _)| SavedEntry {
            path: slashed(q.path()),
            connection: q.connection().map(str::to_owned),
        })
        .collect();
    let truncated = queries.len() > MAX_LISTED;
    queries.truncate(MAX_LISTED);
    Ok(SavedList { queries, truncated })
}

/// The text of one saved query, cut to the byte budget.
pub(crate) async fn read(server: &DbentoServer, args: ReadArgs) -> Result<SavedText, ToolError> {
    let ws = server.workspace().await?;
    let rel = Path::new(&args.path).to_path_buf();
    let is_query = rel.extension().is_some_and(|e| {
        ["sql", "js", "surql", "redis", "partiql"]
            .iter()
            .any(|x| e.eq_ignore_ascii_case(x))
    });
    if !is_query {
        return Err(outside(&args.path));
    }
    // The first folder binds a connection only when the workspace configures one by that name.
    let connection = bound_connection(&ws, &rel);
    if connection.as_deref().is_some_and(|c| !server.exposed(c)) {
        return Err(no_query(&args.path));
    }
    let shown = args.path.clone();
    let text = blocking(move || {
        queries_inside(&ws)?;
        let target = target_connection(&ws, &rel);
        Ok((target, ws.read_query_at(&rel)))
    })
    .await?;
    // A link elsewhere inside queries/ is judged by the folder of the file it reaches too.
    let text = match text {
        (Some(target), _) if !server.exposed(&target) => Err(no_query(&shown)),
        (_, read) => match read {
            Ok(Some(text)) => Ok(text),
            Ok(None) => Err(no_query(&shown)),
            Err(QueryFileError::OutsideQueries { .. }) => Err(outside(&shown)),
            // Core's messages name paths relative to the workspace, never the local folder.
            Err(e) => Err(ToolError::new("query_file", e.to_string())),
        },
    }?;
    let budget = usize::try_from(BYTE_BUDGET).unwrap_or(usize::MAX);
    let truncated = text.len() > budget;
    let sql = if truncated {
        text[..text.floor_char_boundary(budget)].to_owned()
    } else {
        text
    };
    Ok(SavedText {
        path: args.path,
        connection,
        sql,
        truncated,
    })
}

/// Refuses a `queries/` folder that resolves outside the workspace; a missing one is fine.
fn queries_inside(ws: &Workspace) -> Result<(), ToolError> {
    match std::fs::canonicalize(ws.dir().join("queries")) {
        Ok(real) if real.starts_with(ws.dir()) => Ok(()),
        Ok(_) => Err(outside("queries")),
        Err(e) if e.kind() == ErrorKind::NotFound => Ok(()),
        Err(_) => Err(unreadable()),
    }
}

/// The connection bound by the folder of the file `rel` resolves to, links followed; None
/// when it binds none or does not resolve inside `queries/`.
fn target_connection(ws: &Workspace, rel: &Path) -> Option<String> {
    let root = std::fs::canonicalize(ws.dir().join("queries")).ok()?;
    let real = std::fs::canonicalize(ws.dir().join(rel)).ok()?;
    let below = real.strip_prefix(&root).ok()?;
    bound_connection(ws, &Path::new("queries").join(below))
}

/// The configured connection named by the first folder below `queries/`, as core binds it.
fn bound_connection(ws: &Workspace, rel: &Path) -> Option<String> {
    let parts: Vec<Component> = rel.components().collect();
    match parts.as_slice() {
        [Component::Normal(q), Component::Normal(folder), _, ..] if *q == "queries" => folder
            .to_str()
            .filter(|name| ws.connection(name).is_some())
            .map(str::to_owned),
        _ => None,
    }
}

/// The path with `/` between its parts on every platform.
fn slashed(path: &Path) -> String {
    path.components()
        .map(|c| c.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

/// Runs a blocking file operation off the async runtime.
async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, ToolError> + Send + 'static,
) -> Result<T, ToolError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|_| ToolError::new("internal", "reading the queries folder failed".to_owned()))?
}

fn outside(path: &str) -> ToolError {
    ToolError::new(
        "outside_queries",
        format!(
            "{path:?} is not a .sql, .js, .surql, .redis or .partiql file under the workspace's queries/ folder"
        ),
    )
}

fn no_query(path: &str) -> ToolError {
    ToolError::new(
        "no_query",
        format!("no saved query at {path:?}; call list_saved_queries"),
    )
}

fn unreadable() -> ToolError {
    ToolError::new(
        "query_file",
        "the workspace's queries/ folder cannot be read".to_owned(),
    )
}
