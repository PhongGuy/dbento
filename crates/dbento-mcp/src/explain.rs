//! The `explain` tool: the plan of one statement, never its execution.

use dbento_core::{ExplainError, explain_text};
use rmcp::RoleServer;
use rmcp::service::RequestContext;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::ToolError;
use crate::query::{QueryResult, one_statement, run_read};
use crate::server::DbentoServer;

/// `explain` input.
#[derive(Deserialize, JsonSchema)]
pub(crate) struct ExplainArgs {
    /// The connection's name, as `list_connections` gives it.
    connection: String,
    /// Exactly one SELECT, WITH, VALUES, TABLE, INSERT, UPDATE, DELETE, MERGE or REPLACE
    /// statement, without any EXPLAIN prefix or leading block comment; it is planned, never run.
    sql: String,
}

/// `explain` output: the statement that ran and the plan rows it returned.
#[derive(Serialize, JsonSchema)]
pub(crate) struct ExplainResult {
    /// The text sent to the database: `EXPLAIN <sql>`, or `EXPLAIN QUERY PLAN <sql>` on SQLite.
    sql: String,
    #[serde(flatten)]
    plan: QueryResult,
}

/// Plans `args.sql` with plain EXPLAIN in a forced read-only session, as `query` would run it.
pub(crate) async fn run(
    server: &DbentoServer,
    args: ExplainArgs,
    context: &RequestContext<RoleServer>,
) -> Result<ExplainResult, ToolError> {
    let found = server.find(&args.connection).await?;
    let sql = explain_text(found.conn.engine(), &args.sql).map_err(|e| match e {
        ExplainError::NotOneStatement => one_statement("explain"),
        ExplainError::UnsupportedEngine => ToolError::new(
            "unsupported",
            format!(
                "`explain` is not available on {} connections yet",
                found.conn.engine().name()
            ),
        ),
        _ => ToolError::new(
            "not_explainable",
            "`explain` plans one SELECT, WITH, VALUES, TABLE, INSERT, UPDATE, DELETE, MERGE or \
             REPLACE statement, given without EXPLAIN, its options or a leading block comment; \
             ANALYZE is never run",
        ),
    })?;
    let plan = run_read(server, &found, &sql, None, context).await?;
    Ok(ExplainResult { sql, plan })
}
