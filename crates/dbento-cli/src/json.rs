//! The streamed JSON document: one line per statement opening, one per row, one per close.
//! A failure the session reports only on close, after every statement was streamed, ends the
//! document as a top-level `"error"` beside `"statements"`.

use std::io::{self, Write};
use std::time::Duration;

use dbento_core::{ColumnInfo, Engine, ExecuteError, Outcome, ResultBatch, Unguarded};

use crate::cell::cell_text;
use crate::confirm::unguarded_words;
use crate::exec::Sink;

/// Writes `{"dbento":1,...}` to `out` as events arrive, flushing after every batch and statement.
pub(crate) struct JsonSink<W: Write> {
    out: W,
    engine: Engine,
    wrote_statement: bool,
    /// Rows written for the statement in progress; `None` before `started` or after its close.
    open_rows: Option<u64>,
}

impl<W: Write> JsonSink<W> {
    pub(crate) fn new(out: W, engine: Engine) -> Self {
        Self {
            out,
            engine,
            wrote_statement: false,
            open_rows: None,
        }
    }

    /// Separates this statement from the previous one.
    fn next_statement(&mut self) -> io::Result<()> {
        if self.wrote_statement {
            self.out.write_all(b",\n")?;
        }
        self.wrote_statement = true;
        Ok(())
    }

    /// Closes the open `rows` array and writes the counters every row statement carries.
    fn close_rows(
        &mut self,
        rows: u64,
        truncated: bool,
        affected: Option<u64>,
        elapsed: Duration,
    ) -> io::Result<()> {
        let written = self.open_rows.take().unwrap_or(0);
        if written > 0 {
            self.out.write_all(b"\n")?;
        }
        let affected = affected.map_or_else(|| "null".to_owned(), |n| n.to_string());
        write!(
            self.out,
            "],\"row_count\":{rows},\"truncated\":{truncated},\"rows_affected\":{affected},\"duration_ms\":{}",
            elapsed.as_millis()
        )
    }
}

fn quote(s: &str) -> String {
    serde_json::to_string(s).expect("a str always serializes")
}

/// Writes the `"error"` object; an error with its own SQLSTATE (MySQL) also carries it, with
/// the native code repeated as `error_number`, so Postgres and SQLite output stays unchanged.
fn write_error(out: &mut impl Write, e: &ExecuteError, engine: Engine) -> io::Result<()> {
    let (code, message) = error_code(e, engine);
    write!(
        out,
        "{{\"code\":{},\"message\":{}",
        quote(&code),
        quote(&message)
    )?;
    if let Some(sqlstate) = error_sqlstate(e) {
        write!(
            out,
            ",\"sqlstate\":{},\"error_number\":{}",
            quote(sqlstate),
            quote(&code)
        )?;
    }
    out.write_all(b"}")
}

/// The error as the CLI words it for `engine`: a write stopped at the drain cap names the
/// flag that avoids the drain.
pub(crate) fn described(e: &ExecuteError, engine: Engine) -> String {
    let message = e.message_for(engine);
    match e {
        ExecuteError::StoppedAtDrainCap => format!("{message}; use --read-only for plain reads"),
        _ => message,
    }
}

/// The SQLSTATE of a database error that carries one (MySQL); None otherwise.
pub(crate) fn error_sqlstate(e: &ExecuteError) -> Option<&str> {
    match e {
        ExecuteError::Database { sqlstate, .. } => sqlstate.as_deref(),
        _ => None,
    }
}

/// The `(code, message)` pair reported for a failed statement, worded for `engine`.
pub(crate) fn error_code(e: &ExecuteError, engine: Engine) -> (String, String) {
    match e {
        ExecuteError::Database { code, message, .. } => (code.clone(), message.clone()),
        ExecuteError::Timeout => ("timeout".to_owned(), e.message_for(engine)),
        ExecuteError::Cancelled => ("cancelled".to_owned(), e.message_for(engine)),
        ExecuteError::ReadOnlyViolation { message } => ("read_only".to_owned(), message.clone()),
        ExecuteError::Unsupported { reason } => ("unsupported".to_owned(), reason.clone()),
        // The CLI's JSON contract predates the split; transaction control stays "unsupported".
        ExecuteError::TransactionControl { reason } => ("unsupported".to_owned(), reason.clone()),
        ExecuteError::Syntax { .. } => ("syntax".to_owned(), e.message_for(engine)),
        ExecuteError::SessionClosed | ExecuteError::SessionEnded { .. } => {
            ("session_closed".to_owned(), e.message_for(engine))
        }
        // The session is gone either way, so the code stays; the message says why.
        ExecuteError::StoppedAtDrainCap => ("session_closed".to_owned(), described(e, engine)),
        _ => ("error".to_owned(), e.message_for(engine)),
    }
}

/// The JSON text for a document's relaxed Extended JSON: as is when it is an object, else
/// quoted as a string (an undecodable document renders as a comment).
fn json_document_text(ext_json: &str) -> String {
    if ext_json.starts_with('{') {
        ext_json.to_owned()
    } else {
        quote(ext_json)
    }
}

impl<W: Write> Sink for JsonSink<W> {
    fn begin(&mut self, connection: &str) -> io::Result<()> {
        writeln!(
            self.out,
            "{{\"dbento\":1,\"connection\":{},\"statements\":[",
            quote(connection)
        )?;
        self.out.flush()
    }

    fn started(&mut self, index: usize, sql: &str, columns: &[ColumnInfo]) -> io::Result<()> {
        self.next_statement()?;
        let columns: Vec<String> = columns
            .iter()
            .map(|c| {
                format!(
                    "{{\"name\":{},\"type\":{}}}",
                    quote(c.name()),
                    quote(c.type_name())
                )
            })
            .collect();
        write!(
            self.out,
            "{{\"index\":{index},\"sql\":{},\"columns\":[{}],\"rows\":[",
            quote(sql),
            columns.join(",")
        )?;
        self.open_rows = Some(0);
        self.out.flush()
    }

    fn batch(&mut self, batch: &ResultBatch) -> io::Result<()> {
        let mut written = self.open_rows.unwrap_or(0);
        let mut line = String::new();
        for row in 0..batch.len() {
            line.clear();
            line.push_str(if written == 0 { "\n" } else { ",\n" });
            // A row with a document wraps its cells in an object that carries the document too.
            let document = batch.document(row);
            if document.is_some() {
                line.push_str("{\"cells\":");
            }
            line.push('[');
            for col in 0..batch.column_count() {
                if col > 0 {
                    line.push(',');
                }
                match cell_text(batch.cell(row, col))? {
                    Some(text) => line.push_str(&quote(&text)),
                    None => line.push_str("null"),
                }
            }
            line.push(']');
            if let Some(document) = document {
                line.push_str(",\"document\":");
                line.push_str(&json_document_text(&document.to_extended_json(false)));
                line.push('}');
            }
            self.out.write_all(line.as_bytes())?;
            written += 1;
        }
        self.open_rows = Some(written);
        self.out.flush()
    }

    fn finished(&mut self, outcome: &Outcome, elapsed: Duration) -> io::Result<()> {
        self.close_rows(
            outcome.rows,
            outcome.truncated,
            outcome.rows_affected,
            elapsed,
        )?;
        // Written only when there were any, so a single-result statement reads as before.
        if outcome.extra_results > 0 {
            write!(self.out, ",\"extra_results\":{}", outcome.extra_results)?;
        }
        if outcome.rolled_back {
            self.out.write_all(b",\"rolled_back\":true")?;
        }
        self.out.write_all(b",\"status\":\"ok\"}")?;
        self.out.flush()
    }

    fn failed(
        &mut self,
        index: usize,
        sql: Option<&str>,
        error: &ExecuteError,
        elapsed: Duration,
    ) -> io::Result<()> {
        // A failure mid-stream keeps the rows already written and closes them off first.
        if let Some(rows) = self.open_rows {
            self.close_rows(rows, false, None, elapsed)?;
        } else {
            self.next_statement()?;
            let sql = sql.map_or_else(|| "null".to_owned(), quote);
            write!(self.out, "{{\"index\":{index},\"sql\":{sql}")?;
        }
        self.out.write_all(b",\"status\":\"error\",\"error\":")?;
        write_error(&mut self.out, error, self.engine)?;
        self.out.write_all(b"}")?;
        self.out.flush()
    }

    fn refused(&mut self, index: usize, sql: &str, kind: Unguarded) -> io::Result<()> {
        self.next_statement()?;
        let (_, reason) = unguarded_words(kind);
        write!(
            self.out,
            "{{\"index\":{index},\"sql\":{},\"status\":\"refused\",\"reason\":{}}}",
            quote(sql),
            quote(reason)
        )?;
        self.out.flush()
    }

    fn end(&mut self, closing: Option<&ExecuteError>) -> io::Result<()> {
        if self.wrote_statement {
            self.out.write_all(b"\n")?;
        }
        self.out.write_all(b"]")?;
        // The statements were streamed before the close, so its failure is a run-level field.
        if let Some(e) = closing {
            self.out.write_all(b",\"error\":")?;
            write_error(&mut self.out, e, self.engine)?;
        }
        self.out.write_all(b"}\n")?;
        self.out.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_document_that_will_not_decode_is_quoted() {
        assert_eq!(json_document_text("{\"a\":1}"), "{\"a\":1}");
        assert_eq!(
            json_document_text("/* undecodable document: x */"),
            "\"/* undecodable document: x */\""
        );
    }

    #[test]
    fn a_session_that_ended_keeps_the_code_and_says_why() {
        let e = ExecuteError::SessionEnded {
            reason: "the server went away".into(),
        };
        let (code, message) = error_code(&e, Engine::Surrealdb);
        assert_eq!(code, "session_closed");
        assert!(message.contains("the server went away"), "{message}");
    }

    #[test]
    fn transaction_control_keeps_reporting_as_unsupported() {
        let e = ExecuteError::TransactionControl {
            reason: "not in a read-only session".into(),
        };
        assert_eq!(
            error_code(&e, Engine::Postgres),
            (
                "unsupported".to_owned(),
                "not in a read-only session".to_owned()
            )
        );
    }

    #[test]
    fn a_write_stopped_at_the_cap_is_the_run_level_error() {
        let mut out = Vec::new();
        let mut sink = JsonSink::new(&mut out, Engine::Postgres);
        sink.begin("pg").unwrap();
        sink.end(Some(&ExecuteError::StoppedAtDrainCap)).unwrap();
        let doc: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(doc["error"]["code"], "session_closed");
        assert_eq!(
            doc["error"]["message"],
            "the statement was still running when the row limit was hit, so it was stopped \
             and any writes it made were rolled back; use --read-only for plain reads"
        );
    }

    #[test]
    fn a_mysql_write_stopped_at_the_cap_says_a_non_transactional_write_may_stay() {
        let mut out = Vec::new();
        let mut sink = JsonSink::new(&mut out, Engine::Mysql);
        sink.begin("my").unwrap();
        sink.end(Some(&ExecuteError::StoppedAtDrainCap)).unwrap();
        let doc: serde_json::Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(doc["error"]["code"], "session_closed");
        assert_eq!(
            doc["error"]["message"],
            "the statement was still running when the row limit was hit, so it was stopped; \
             a transactional write was rolled back, but a non-transactional one may have kept \
             part of its changes; use --read-only for plain reads"
        );
    }

    #[test]
    fn an_error_without_a_sqlstate_writes_code_and_message_only() {
        let mut out = Vec::new();
        let mut sink = JsonSink::new(&mut out, Engine::Postgres);
        sink.begin("pg").unwrap();
        let e = ExecuteError::database("42P01", "relation \"x\" does not exist");
        sink.failed(0, Some("select * from x"), &e, Duration::from_millis(3))
            .unwrap();
        sink.end(None).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            concat!(
                r#"{"dbento":1,"connection":"pg","statements":["#,
                "\n",
                r#"{"index":0,"sql":"select * from x","status":"error","error":{"code":"42P01","message":"relation \"x\" does not exist"}}"#,
                "\n]}\n"
            )
        );
    }
}
