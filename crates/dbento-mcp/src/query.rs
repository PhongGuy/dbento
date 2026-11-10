//! The `query` tool: one statement in a read-only session, bounded by rows and bytes.

use std::time::{Instant, SystemTime};

use dbento_core::{
    CellRef, Classification, ColumnInfo, ExecuteError, HistoryEntry, HistoryOutcome, HistorySource,
    Limits, OnTruncate, Outcome, Pending, ReadOnlyEnforcement, Script, TruncatedBy,
    statement_count,
};
use rmcp::RoleServer;
use rmcp::service::RequestContext;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::ToolError;
use crate::server::{DbentoServer, Found};
use crate::session::{enforcement_name, record, until_cancelled};

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
    /// Exactly one statement in the connection's query language (`language` in
    /// list_connections); it runs in a read-only session.
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
    /// Each returned row's stored document as relaxed Extended JSON text, cut like a cell;
    /// absent on engines whose rows carry none.
    #[serde(skip_serializing_if = "Option::is_none")]
    documents: Option<Vec<Value>>,
    row_count: u64,
    /// More rows existed than were returned.
    truncated: bool,
    /// The bound that cut the result, or null when it is whole.
    truncated_by: Option<Bound>,
    /// Results after the first that ran to their end and are not returned; 0 after a cut, which
    /// stops the batch.
    extra_results: u32,
    /// The statement's changes did not persist: SurrealDB ran it in a transaction it cancelled.
    rolled_back: bool,
    /// Who enforced this read-only session: "database", "best_effort" or "unknown".
    read_only_enforcement: &'static str,
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
    if statement_count(found.conn.engine(), &args.sql) != 1 {
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
    let engine = opened.session.engine();
    let cancel = opened.session.cancel_handle();
    let work = collect(&mut opened.session, sql, limits);
    let collected = until_cancelled(context, &cancel, engine, work).await;
    let elapsed = started.elapsed();
    let enforcement = opened.session.read_only_enforcement();
    // A cut stops the batch, so what it did is known only as the session closes. Where the
    // database cannot refuse writes, any error then means the batch did not finish cleanly, so
    // what it wrote is unknown and success would claim otherwise.
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
            history_outcome(collected.as_ref().map(|(_, _, _, o)| o)),
        )
        .with_source(HistorySource::Mcp);
        record(path, entry).await;
    }
    let (columns, rows, documents, outcome) = collected?;
    let truncated_by = match outcome.truncated_by {
        None => None,
        Some(TruncatedBy::Rows) => Some(Bound::Rows),
        Some(TruncatedBy::Bytes) => Some(Bound::Bytes),
        Some(_) => {
            return Err(ToolError::new(
                "internal",
                "an unknown truncation bound".to_owned(),
            ));
        }
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
        documents,
        row_count: outcome.rows,
        truncated: outcome.truncated,
        truncated_by,
        extra_results: outcome.extra_results,
        rolled_back: outcome.rolled_back,
        read_only_enforcement: enforcement.map_or("unknown", enforcement_name),
        duration_ms: u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX),
    })
}

// Server categories `query` refuses even in a read-only session: they scan the whole keyspace,
// read server state, run scripts or hold the connection open waiting.
const REFUSED_CATEGORIES: [&str; 4] = ["@dangerous", "@blocking", "@scripting", "@pubsub"];

/// The first of the refused categories the statement's command belongs to, if any.
pub(crate) fn refused_category(c: &Classification) -> Option<&'static str> {
    REFUSED_CATEGORIES
        .into_iter()
        .find(|refused| c.categories.iter().any(|have| have == refused))
}

/// Executes the script's only statement, walks every batch into JSON rows (and the rows'
/// documents, when the first row carries one) and, unless a cut stopped it, reads the rest of
/// the script to its end.
async fn collect(
    session: &mut dbento_core::Session,
    sql: &str,
    limits: Limits,
) -> Result<Collected, ToolError> {
    let engine = session.engine();
    let enforcement = session.read_only_enforcement();
    let failed = |e: ExecuteError| ToolError::from_execute_on(&e, engine);
    let mut script = session.script(sql);
    let pending = match script.next().await {
        None => return Err(one_statement("query")),
        Some(p) => p.map_err(failed)?,
    };
    if let Some(category) = refused_category(pending.classification()) {
        let command = pending
            .sql()
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_uppercase();
        pending.skip();
        return Err(ToolError::new(
            "refused_category",
            format!(
                "`{command}` is in the server's {category} category, which `query` refuses; \
                 walk keys with `SCAN 0 MATCH <pattern> COUNT 100` and `TYPE <key>`"
            ),
        ));
    }
    let mut stream = pending.execute(limits).await.map_err(failed)?;
    let mut rows = Vec::new();
    let mut documents: Option<Vec<Value>> = None;
    while let Some(batch) = stream.next_batch().await.map_err(failed)? {
        for r in 0..batch.len() {
            // Whether rows carry documents is the engine's, so the first row decides for all.
            let document = batch
                .document(r)
                .map(|d| cell(CellRef::Text(&d.to_extended_json(false))))
                .transpose()?;
            match (&mut documents, document) {
                (Some(docs), Some(d)) => docs.push(d),
                (None, Some(d)) if rows.is_empty() => documents = Some(vec![d]),
                _ => {}
            }
            rows.push(
                (0..batch.column_count())
                    .map(|c| cell(batch.cell(r, c)))
                    .collect::<Result<_, _>>()?,
            );
        }
    }
    let mut outcome = stream.outcome().cloned().ok_or_else(|| {
        ToolError::new("internal", "the result ended without an outcome".to_owned())
    })?;
    let columns = stream.columns().to_vec();
    drop(stream);
    // A cut under Close ended the session, so nothing is left to read.
    if !outcome.truncated {
        let extra = discard(&mut script, enforcement).await.map_err(failed)?;
        outcome.extra_results = outcome.extra_results.saturating_add(extra);
    }
    Ok((columns, rows, documents, outcome))
}

/// A collected result: its columns, rows, documents (on engines with them) and outcome.
type Collected = (
    Vec<ColumnInfo>,
    Vec<Vec<Value>>,
    Option<Vec<Value>>,
    Outcome,
);

/// Reads the script's later results to their end, keeping no rows, and counts those with
/// columns. An error fails the call on a best-effort session or as a read-only violation, where
/// what the batch wrote is unknown or refused; otherwise the database refused every write, so
/// counting stops and the first result stands.
async fn discard(
    script: &mut Script<'_>,
    enforcement: Option<ReadOnlyEnforcement>,
) -> Result<u32, ExecuteError> {
    let mut extra = 0_u32;
    while let Some(next) = script.next().await {
        let read = match next {
            Ok(pending) => read_to_end(pending).await,
            Err(e) => Err(e),
        };
        match read {
            Ok(counted) => extra = extra.saturating_add(counted),
            Err(e)
                if enforcement == Some(ReadOnlyEnforcement::BestEffort)
                    || matches!(e, ExecuteError::ReadOnlyViolation { .. }) =>
            {
                return Err(e);
            }
            Err(_) => break,
        }
    }
    Ok(extra)
}

/// Runs one later statement to its end and answers how many results it counts for. No row is
/// kept and Drain sends no Attention, so the statement finishes as written.
async fn read_to_end(pending: Pending<'_>) -> Result<u32, ExecuteError> {
    let limits = {
        let mut l = Limits::default();
        l.max_rows = Some(0);
        l.on_truncate = OnTruncate::Drain;
        l
    };
    let mut stream = pending.execute(limits).await?;
    while stream.next_batch().await?.is_some() {}
    let has_columns = !stream.columns().is_empty();
    let inner = stream.outcome().map_or(0, |o| o.extra_results);
    Ok(u32::from(has_columns).saturating_add(inner))
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
                "a cell kind this server does not know".to_owned(),
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
