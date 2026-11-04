//! The `query` tool: one statement in a read-only session, bounded by rows and bytes.

use std::time::{Instant, SystemTime};

use dbento_core::{
    CellRef, ColumnInfo, ExecuteError, HistoryEntry, HistoryOutcome, HistorySource, Limits,
    OnTruncate, Outcome, ReadOnlyEnforcement, TruncatedBy, statement_ranges,
};
use rmcp::RoleServer;
use rmcp::service::RequestContext;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::ToolError;
use crate::server::{DbentoServer, Found};
use crate::session::{record, until_cancelled};

/// Rows returned when the client names no `max_rows`.
pub(crate) const DEFAULT_ROWS: u64 = 100;
/// The most rows one call returns, whatever the client asks for.
pub(crate) const MAX_ROWS: u64 = 1000;
/// Cell bytes after which no further row is collected.
pub(crate) const BYTE_BUDGET: u64 = 64 << 10;
/// The longest cell, in bytes of text or characters of hex, returned whole.
pub(crate) const CELL_CUT: usize = 4 << 10;

/// `query` input.
#[derive(Deserialize, JsonSchema)]
pub(crate) struct QueryArgs {
    /// The connection's name, as `list_connections` gives it.
    connection: String,
    /// Exactly one SQL statement; it runs in a read-only session.
    sql: String,
    /// default 100, at most 1000
    max_rows: Option<u64>,
}

/// `query` output: the columns, the rows as cells, and how the result ended.
#[derive(Serialize, JsonSchema)]
pub(crate) struct QueryResult {
    columns: Vec<ResultColumn>,
    /// Each cell is a string, null, `{"hex"}` for bytes, or `{"text"|"hex", "cut": true}`
    /// when it was longer than 4 KiB.
    rows: Vec<Vec<Value>>,
    row_count: u64,
    /// More rows existed than were returned.
    truncated: bool,
    /// The bound that cut the result, or null when it is whole.
    truncated_by: Option<Bound>,
    duration_ms: u64,
}

/// One result column.
#[derive(Serialize, JsonSchema)]
struct ResultColumn {
    name: String,
    /// The type as the engine names it; empty when SQLite declares none.
    #[serde(rename = "type")]
    type_name: String,
}

/// The bound that cut a result short.
#[derive(Serialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
enum Bound {
    /// `max_rows` was reached.
    Rows,
    /// The 64 KiB byte budget was reached.
    Bytes,
}

/// Runs `args.sql` as one read-only statement and collects its bounded result.
pub(crate) async fn run(
    server: &DbentoServer,
    args: QueryArgs,
    context: &RequestContext<RoleServer>,
) -> Result<QueryResult, ToolError> {
    // The engine decides the split, so the lookup comes first; neither one connects.
    let found = server.find(&args.connection).await?;
    if statement_ranges(found.conn.engine(), &args.sql).len() != 1 {
        return Err(one_statement("query"));
    }
    run_read(server, &found, &args.sql, args.max_rows, context).await
}

/// Runs `sql`, already checked to be one statement, in a forced read-only session on
/// `found`, records it as MCP history and collects its bounded result.
pub(crate) async fn run_read(
    server: &DbentoServer,
    found: &Found,
    sql: &str,
    max_rows: Option<u64>,
    context: &RequestContext<RoleServer>,
) -> Result<QueryResult, ToolError> {
    let mut opened = server.open(found).await?;
    let limits = {
        let mut l = Limits::default();
        l.max_rows = Some(max_rows.unwrap_or(DEFAULT_ROWS).min(MAX_ROWS));
        l.max_bytes = Some(BYTE_BUDGET);
        l.on_truncate = OnTruncate::Close;
        l
    };
    let started_at = SystemTime::now();
    let started = Instant::now();
    let cancel = opened.session.cancel_handle();
    let work = collect(&mut opened.session, sql, limits);
    let collected = until_cancelled(context, &cancel, work).await;
    let elapsed = started.elapsed();
    let engine = opened.session.engine();
    let enforcement = opened.session.read_only_enforcement();
    // A script ends at its first result here, so what the rest of the batch did is known only as
    // the session closes. Where the database cannot refuse writes, any error then means the batch
    // did not finish cleanly, so what it wrote is unknown and success would claim otherwise.
    let collected = match (opened.session.close().await, collected) {
        (Err(e @ ExecuteError::ReadOnlyViolation { .. }), _) => Err(ToolError::from_execute(&e)),
        (Err(e), Ok(_)) if enforcement == Some(ReadOnlyEnforcement::BestEffort) => {
            Err(ToolError::from_execute_on(&e, engine))
        }
        (_, collected) => collected,
    };
    // Once a session opened, the statement is recorded however it ended.
    if let Some(path) = server.history_file() {
        let entry = HistoryEntry::new(
            started_at,
            &opened.dir,
            opened.conn.name(),
            sql,
            elapsed,
            history_outcome(collected.as_ref().map(|(_, _, o)| o)),
        )
        .with_source(HistorySource::Mcp);
        record(path, entry).await;
    }
    let (columns, rows, outcome) = collected?;
    let truncated_by = match outcome.truncated_by {
        None => None,
        Some(TruncatedBy::Rows) => Some(Bound::Rows),
        Some(TruncatedBy::Bytes) => Some(Bound::Bytes),
        Some(_) => return Err(ToolError::new("internal", "an unknown truncation bound")),
    };
    Ok(QueryResult {
        columns: columns
            .iter()
            .map(|c| ResultColumn {
                name: c.name().to_owned(),
                type_name: c.type_name().to_owned(),
            })
            .collect(),
        rows,
        row_count: outcome.rows,
        truncated: outcome.truncated,
        truncated_by,
        duration_ms: u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
    })
}

/// Executes the script's only statement and walks every batch into JSON rows.
async fn collect(
    session: &mut dbento_core::Session,
    sql: &str,
    limits: Limits,
) -> Result<(Vec<ColumnInfo>, Vec<Vec<Value>>, Outcome), ToolError> {
    let engine = session.engine();
    let failed = |e: ExecuteError| ToolError::from_execute_on(&e, engine);
    let mut script = session.script(sql);
    let pending = match script.next().await {
        None => return Err(one_statement("query")),
        Some(p) => p.map_err(failed)?,
    };
    let mut stream = pending.execute(limits).await.map_err(failed)?;
    let mut rows = Vec::new();
    while let Some(batch) = stream.next_batch().await.map_err(failed)? {
        for r in 0..batch.len() {
            rows.push(
                (0..batch.column_count())
                    .map(|c| cell(batch.cell(r, c)))
                    .collect::<Result<_, _>>()?,
            );
        }
    }
    let outcome = stream
        .outcome()
        .cloned()
        .ok_or_else(|| ToolError::new("internal", "the result ended without an outcome"))?;
    Ok((stream.columns().to_vec(), rows, outcome))
}

/// How a finished (or failed) statement reads in the history.
pub(crate) fn history_outcome(ended: Result<&Outcome, &ToolError>) -> HistoryOutcome {
    match ended {
        Ok(o) => HistoryOutcome::Ok {
            rows: o.rows,
            truncated: o.truncated,
            rows_affected: o.rows_affected,
        },
        Err(e) if e.code() == "cancelled" => HistoryOutcome::Cancelled,
        Err(e) => HistoryOutcome::Failed {
            message: e.message().to_owned(),
        },
    }
}

/// The refusal of a script that is not exactly one statement, naming the `tool` called.
pub(crate) fn one_statement(tool: &str) -> ToolError {
    ToolError::new(
        "one_statement",
        format!("`{tool}` runs exactly one statement; send each statement in its own call"),
    )
}

/// A string, null, {"hex"}, or {"text"|"hex", "cut": true} past CELL_CUT.
pub(crate) fn cell(c: CellRef<'_>) -> Result<Value, ToolError> {
    Ok(match c {
        CellRef::Null => Value::Null,
        CellRef::Text(t) if t.len() <= CELL_CUT => json!(t),
        CellRef::Text(t) => json!({"text": &t[..t.floor_char_boundary(CELL_CUT)], "cut": true}),
        // Two hex characters per byte, so only the bytes that fit are encoded.
        CellRef::Bytes(b) if b.len() * 2 <= CELL_CUT => json!({"hex": hex(b)}),
        CellRef::Bytes(b) => json!({"hex": hex(&b[..CELL_CUT / 2]), "cut": true}),
        // Never null for a kind this server does not know; the call fails and still closes.
        _ => {
            return Err(ToolError::new(
                "internal",
                "a cell kind this server does not know",
            ));
        }
    })
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(s, "{b:02x}");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn cells_are_strings_null_hex_or_cut_on_a_char_boundary() {
        assert_eq!(cell(CellRef::Null).unwrap(), json!(null));
        assert_eq!(cell(CellRef::Text("a")).unwrap(), json!("a"));
        assert_eq!(
            cell(CellRef::Bytes(&[0, 255])).unwrap(),
            json!({"hex": "00ff"})
        );
        let exact = "z".repeat(CELL_CUT);
        assert_eq!(cell(CellRef::Text(&exact)).unwrap(), json!(exact));
        let straddling = format!("{}é", "a".repeat(CELL_CUT - 1));
        assert_eq!(
            cell(CellRef::Text(&straddling)).unwrap(),
            json!({"text": "a".repeat(CELL_CUT - 1), "cut": true})
        );
        let blob = vec![0xab; CELL_CUT];
        assert_eq!(
            cell(CellRef::Bytes(&blob)).unwrap(),
            json!({"hex": "ab".repeat(CELL_CUT / 2), "cut": true})
        );
    }
}
