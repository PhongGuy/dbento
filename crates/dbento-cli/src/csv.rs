//! CSV output for `--format csv`, written the way Postgres `COPY ... CSV` does.

use std::io::{self, Write};
use std::time::Duration;

use dbento_core::{ColumnInfo, Engine, ExecuteError, Outcome, ResultBatch, Unguarded};

use crate::cell::cell_text;
use crate::exec::Sink;
use crate::table::{error_note, more_results, refused_note};

/// Streams each result as CSV on `out`; failures and truncation notes go to `err`.
/// NULL is an empty unquoted field and empty text is `""`, so the two stay apart.
pub(crate) struct CsvSink<W: Write, E: Write> {
    out: W,
    err: E,
    engine: Engine,
    printed: bool,
    /// The statement in progress, for the truncation note.
    index: usize,
}

impl<W: Write, E: Write> CsvSink<W, E> {
    pub(crate) fn new(out: W, err: E, engine: Engine) -> Self {
        Self {
            out,
            err,
            engine,
            printed: false,
            index: 0,
        }
    }
}

/// Appends `text` to `line`, quoted when empty or when it holds a comma, quote or line break.
fn push_field(line: &mut String, text: &str) {
    if text.is_empty() || text.contains([',', '"', '\r', '\n']) {
        line.push('"');
        line.push_str(&text.replace('"', "\"\""));
        line.push('"');
    } else {
        line.push_str(text);
    }
}

impl<W: Write, E: Write> Sink for CsvSink<W, E> {
    fn begin(&mut self, _connection: &str) -> io::Result<()> {
        Ok(())
    }

    fn started(&mut self, index: usize, _sql: &str, columns: &[ColumnInfo]) -> io::Result<()> {
        self.index = index;
        if columns.is_empty() {
            return Ok(());
        }
        let mut line = String::new();
        if self.printed {
            line.push('\n');
        }
        for (i, c) in columns.iter().enumerate() {
            if i > 0 {
                line.push(',');
            }
            push_field(&mut line, c.name());
        }
        line.push('\n');
        self.printed = true;
        self.out.write_all(line.as_bytes())?;
        self.out.flush()
    }

    fn batch(&mut self, batch: &ResultBatch) -> io::Result<()> {
        let mut text = String::new();
        for row in 0..batch.len() {
            for col in 0..batch.column_count() {
                if col > 0 {
                    text.push(',');
                }
                if let Some(field) = cell_text(batch.cell(row, col))? {
                    push_field(&mut text, &field);
                }
            }
            text.push('\n');
        }
        self.out.write_all(text.as_bytes())?;
        self.out.flush()
    }

    fn finished(&mut self, outcome: &Outcome, _elapsed: Duration) -> io::Result<()> {
        if outcome.truncated {
            writeln!(
                self.err,
                "statement {} truncated by --max-rows",
                self.index + 1
            )?;
        }
        if let Some(more) = more_results(outcome.extra_results) {
            writeln!(self.err, "statement {}: {more}", self.index + 1)?;
        }
        Ok(())
    }

    fn failed(
        &mut self,
        index: usize,
        _sql: Option<&str>,
        error: &ExecuteError,
        _elapsed: Duration,
    ) -> io::Result<()> {
        writeln!(self.err, "{}", error_note(index, error, self.engine))
    }

    fn refused(&mut self, index: usize, _sql: &str, kind: Unguarded) -> io::Result<()> {
        writeln!(self.err, "{}", refused_note(index, kind))
    }

    /// A closing failure is already on stderr, where these formats report every error.
    fn end(&mut self, _closing: Option<&ExecuteError>) -> io::Result<()> {
        self.out.flush()
    }
}
