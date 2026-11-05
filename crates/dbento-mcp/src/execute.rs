//! The `execute` tool: one write or DDL statement, run only after the user approves the call.

use std::path::PathBuf;
use std::time::{Instant, SystemTime};

use dbento_core::{
    ApprovalRequest, ApprovalStore, Claim, Classification, ExecuteError, HistoryEntry,
    HistorySource, Limits, Outcome, Session, Writes, statement_count,
};
use rmcp::RoleServer;
use rmcp::service::RequestContext;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::ToolError;
use crate::query::{history_outcome, one_statement};
use crate::server::{DbentoServer, Found};
use crate::session::{record, until_cancelled};

/// `execute` input.
#[derive(Deserialize, JsonSchema)]
pub(crate) struct ExecuteArgs {
    /// The connection's name, as `list_connections` gives it.
    connection: String,
    /// Exactly one write or DDL statement in the connection's query language; an approval
    /// covers this exact text only.
    sql: String,
}

/// `execute` output.
#[derive(Serialize, JsonSchema)]
pub(crate) struct ExecuteResult {
    /// Rows the statement changed, or null when the engine does not say.
    rows_affected: Option<u64>,
    duration_ms: u64,
}

/// Refuses what can never be approved, then runs a granted call once, answers a denial once,
/// or files an approval request for the exact call.
pub(crate) async fn run(
    server: &DbentoServer,
    args: ExecuteArgs,
    context: &RequestContext<RoleServer>,
) -> Result<ExecuteResult, ToolError> {
    let found = server.find(&args.connection).await?;
    if found.conn.read_only() {
        return Err(ToolError::new(
            "read_only_connection",
            format!(
                "connection {:?} is read-only; `execute` can never run on it",
                args.connection
            ),
        ));
    }
    if statement_count(found.conn.engine(), &args.sql) != 1 {
        return Err(one_statement("execute"));
    }
    let classification = classify(server, &found, &args, context).await?;
    // SQLite calls `BEGIN` read-only, so transaction control is refused first.
    if classification.transaction_control {
        return Err(ToolError::new(
            "transaction_control",
            "`execute` runs one statement in its own transaction; transaction control is refused",
        ));
    }
    if classification.writes == Writes::No {
        return Err(ToolError::new(
            "not_a_write",
            "the statement only reads; run it with the `query` tool",
        ));
    }
    let Some(dir) = server.approvals().map(PathBuf::from) else {
        return Err(unavailable("this server has no approvals folder"));
    };
    let request = ApprovalRequest::new(
        server.root(),
        &args.connection,
        &args.sql,
        classification.writes,
        classification.unguarded,
    );
    let id = request.id().to_owned();
    // The store's file operations block, so they run off the async runtime. A call with no
    // answer yet files its request in the same hop.
    let claimed = tokio::task::spawn_blocking(move || {
        let store = ApprovalStore::open(&dir)?;
        let claimed = store.claim_with_reason(request.id())?;
        if claimed.0 == Claim::None {
            store.request(&request)?;
        }
        Ok::<_, dbento_core::ApprovalError>(claimed)
    })
    .await
    .map_err(|_| unavailable("reading the approvals folder failed"))?
    .map_err(|e| unavailable(&e.to_string()))?;
    match claimed {
        // The grant is spent before connecting, so a connect or secret failure burns it.
        (Claim::Granted, _) => run_granted(server, &found, &args, context).await,
        (Claim::Denied, reason) => {
            let because = reason.as_ref().map_or(String::new(), |r| format!(": {r}"));
            Err(ToolError::new(
                "denied",
                format!("the user denied request {id}{because}; do not retry it unchanged"),
            )
            .with_request_id(&id)
            .with_reason(reason))
        }
        (Claim::None, _) => Err(ToolError::new(
            "approval_required",
            format!(
                "ask the user to approve request {id} in the Dbento app, then retry the identical call"
            ),
        )
        .with_request_id(&id)),
        // A claim this build does not know must not read as "ask again".
        _ => Err(unavailable("the approval store answered an unknown claim")),
    }
}

/// Runs the approved statement in a read-write session and records it as MCP history.
async fn run_granted(
    server: &DbentoServer,
    found: &Found,
    args: &ExecuteArgs,
    context: &RequestContext<RoleServer>,
) -> Result<ExecuteResult, ToolError> {
    let mut opened = server.open_writable(found).await?;
    let started_at = SystemTime::now();
    let started = Instant::now();
    let engine = opened.session.engine();
    let cancel = opened.session.cancel_handle();
    let work = run_once(&mut opened.session, &args.sql);
    let ran = until_cancelled(context, &cancel, engine, work).await;
    let elapsed = started.elapsed();
    // A write's failed commit can surface only as close drains it, and then the call failed.
    let closed = opened.session.close().await;
    let ran = match (ran, closed) {
        (Ok(_), Err(e)) => Err(ToolError::from_execute_on(&e, engine)),
        (ran, _) => ran,
    };
    if let Some(path) = server.history_file() {
        let entry = HistoryEntry::new(
            started_at,
            &opened.dir,
            opened.conn.name(),
            &args.sql,
            elapsed,
            history_outcome(ran.as_ref()),
        )
        .with_source(HistorySource::Mcp);
        record(path, entry).await;
    }
    Ok(ExecuteResult {
        rows_affected: ran?.rows_affected,
        duration_ms: u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
    })
}

/// Executes the script's only statement, dropping any rows it returns.
async fn run_once(session: &mut Session, sql: &str) -> Result<Outcome, ToolError> {
    let engine = session.engine();
    let failed = |e: ExecuteError| ToolError::from_execute_on(&e, engine);
    let mut script = session.script(sql);
    let pending = match script.next().await {
        None => return Err(one_statement("execute")),
        Some(p) => p.map_err(failed)?,
    };
    let mut stream = pending.execute(Limits::default()).await.map_err(failed)?;
    while stream.next_batch().await.map_err(failed)?.is_some() {}
    stream
        .outcome()
        .cloned()
        .ok_or_else(|| ToolError::new("internal", "the result ended without an outcome"))
}

/// Prepares the statement in a forced read-only session for its classification only.
async fn classify(
    server: &DbentoServer,
    found: &Found,
    args: &ExecuteArgs,
    context: &RequestContext<RoleServer>,
) -> Result<Classification, ToolError> {
    let mut opened = server.open(found).await?;
    let engine = opened.session.engine();
    let cancel = opened.session.cancel_handle();
    let work = async {
        let mut script = opened.session.script(&args.sql);
        match script.next().await {
            None => Err(one_statement("execute")),
            Some(Err(e)) => Err(ToolError::from_execute(&e)),
            Some(Ok(pending)) => {
                let classification = pending.classification().clone();
                pending.skip();
                Ok(classification)
            }
        }
    };
    let classified = until_cancelled(context, &cancel, engine, work).await;
    // A skipped statement leaves nothing for close to report.
    let _ = opened.session.close().await;
    classified
}

/// The `approvals_unavailable` answer. Its message is fixed, so no local path reaches the
/// client; `detail` goes to stderr for the user.
fn unavailable(detail: &str) -> ToolError {
    eprintln!("dbento mcp: approvals are unavailable: {detail}");
    ToolError::new(
        "approvals_unavailable",
        "approvals are unavailable; the Dbento state folder cannot be used",
    )
}
