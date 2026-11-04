//! Plain-text table rendering for `--format table`.

use std::io::{self, Write};
use std::time::Duration;

use unicode_width::UnicodeWidthStr;

use dbento_core::{ColumnInfo, Engine, ExecuteError, Outcome, ResultBatch, Unguarded};

use crate::cell::cell_text;
use crate::confirm::unguarded_words;
use crate::exec::Sink;
use crate::json::{error_code, error_sqlstate};

/// Renders `headers` and `rows` as left-aligned columns joined by ` | `, with a `-+-` rule
/// under the header. Widths are display columns, so CJK and emoji align. Lines are
/// right-trimmed and the result has no trailing newline.
pub(crate) fn render(headers: &[&str], rows: &[Vec<String>]) -> String {
    let mut widths: Vec<usize> = headers.iter().map(|h| h.width()).collect();
    for row in rows {
        for (w, cell) in widths.iter_mut().zip(row) {
            *w = (*w).max(cell.width());
        }
    }
    let line = |cells: Vec<&str>| {
        let padded: Vec<String> = cells
            .iter()
            .zip(&widths)
            .map(|(c, w)| format!("{c}{}", " ".repeat(w.saturating_sub(c.width()))))
            .collect();
        padded.join(" | ").trim_end().to_owned()
    };
    let rule: Vec<String> = widths.iter().map(|w| "-".repeat(*w)).collect();
    let mut lines = vec![line(headers.to_vec()), rule.join("-+-")];
    lines.extend(
        rows.iter()
            .map(|r| line(r.iter().map(String::as_str).collect())),
    );
    lines.join("\n")
}

/// The stderr line for a failed statement; `index` counts from 0, the message from 1.
pub(crate) fn error_note(index: usize, error: &ExecuteError, engine: Engine) -> String {
    let (code, message) = error_code(error, engine);
    match error_sqlstate(error) {
        Some(s) => format!(
            "error: statement {}: {message} ({code}, SQLSTATE {s})",
            index + 1
        ),
        None => format!("error: statement {}: {message} ({code})", index + 1),
    }
}

/// The stderr line for a statement skipped because nobody confirmed its unguarded write.
pub(crate) fn refused_note(index: usize, kind: Unguarded) -> String {
    let (phrase, _) = unguarded_words(kind);
    format!(
        "refused: statement {}: {phrase}; pass --yes to run it",
        index + 1
    )
}

/// The note for result sets a statement returned past its first, or None when there were none.
pub(crate) fn more_results(n: u32) -> Option<String> {
    match n {
        0 => None,
        1 => Some("1 more result set not shown".to_owned()),
        n => Some(format!("{n} more result sets not shown")),
    }
}

/// `1 row` or `3 rows`.
fn rows_phrase(n: u64) -> String {
    format!("{n} row{}", if n == 1 { "" } else { "s" })
}

/// Escapes control characters, ESC and DEL included, so a result cannot inject terminal codes.
fn escape_text(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_ascii_control() => out.push_str(&format!("\\x{:02x}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

/// Prints each result as an aligned table on `out` and the row counts and failures on `err`.
pub(crate) struct TableSink<W: Write, E: Write> {
    out: W,
    err: E,
    engine: Engine,
    printed: bool,
    headers: Vec<String>,
    rows: Vec<Vec<String>>,
    default_cap: bool,
}

impl<W: Write, E: Write> TableSink<W, E> {
    pub(crate) fn new(out: W, err: E, engine: Engine) -> Self {
        Self {
            out,
            err,
            engine,
            printed: false,
            headers: Vec::new(),
            rows: Vec::new(),
            default_cap: false,
        }
    }

    /// Marks the row limit as the table's own default, so a truncation note says how to change it.
    pub(crate) fn with_default_cap(mut self, default_cap: bool) -> Self {
        self.default_cap = default_cap;
        self
    }

    /// Writes the buffered result, if the statement had columns, after a blank separator line.
    fn flush_table(&mut self) -> io::Result<()> {
        if !self.headers.is_empty() {
            if self.printed {
                self.out.write_all(b"\n")?;
            }
            let headers: Vec<&str> = self.headers.iter().map(String::as_str).collect();
            writeln!(self.out, "{}", render(&headers, &self.rows))?;
            self.printed = true;
        }
        self.headers.clear();
        self.rows.clear();
        self.out.flush()
    }
}

impl<W: Write, E: Write> Sink for TableSink<W, E> {
    fn begin(&mut self, _connection: &str) -> io::Result<()> {
        Ok(())
    }

    fn started(&mut self, _index: usize, _sql: &str, columns: &[ColumnInfo]) -> io::Result<()> {
        // Column names come from the server and are as untrusted as cells.
        self.headers = columns.iter().map(|c| escape_text(c.name())).collect();
        self.rows.clear();
        Ok(())
    }

    fn batch(&mut self, batch: &ResultBatch) -> io::Result<()> {
        for row in 0..batch.len() {
            let cells: io::Result<Vec<String>> = (0..batch.column_count())
                // NULL is an empty field; control characters are escaped.
                .map(|col| {
                    Ok(cell_text(batch.cell(row, col))?
                        .map_or_else(String::new, |t| escape_text(&t)))
                })
                .collect();
            self.rows.push(cells?);
        }
        Ok(())
    }

    fn finished(&mut self, outcome: &Outcome, elapsed: Duration) -> io::Result<()> {
        let had_columns = !self.headers.is_empty();
        self.flush_table()?;
        let ms = elapsed.as_millis();
        let more =
            more_results(outcome.extra_results).map_or_else(String::new, |m| format!(", {m}"));
        let more = if outcome.rolled_back {
            format!("{more}, any changes were rolled back")
        } else {
            more
        };
        if had_columns {
            let cut = if outcome.truncated && self.default_cap {
                ", showing the first 1000 rows; pass --max-rows to change"
            } else if outcome.truncated {
                ", truncated by --max-rows"
            } else {
                ""
            };
            writeln!(
                self.err,
                "({}{cut}{more}, {ms} ms)",
                rows_phrase(outcome.rows)
            )
        } else {
            match outcome.rows_affected {
                Some(n) => writeln!(self.err, "OK, {} affected{more} ({ms} ms)", rows_phrase(n)),
                None => writeln!(self.err, "OK{more} ({ms} ms)"),
            }
        }
    }

    fn failed(
        &mut self,
        index: usize,
        _sql: Option<&str>,
        error: &ExecuteError,
        _elapsed: Duration,
    ) -> io::Result<()> {
        // Rows that arrived before the failure are still worth showing.
        self.flush_table()?;
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

#[cfg(test)]
mod tests {
    use super::{error_note, more_results, render};
    use dbento_core::{Engine, ExecuteError};

    fn row(a: &str, b: &str) -> Vec<String> {
        vec![a.to_owned(), b.to_owned()]
    }

    #[test]
    fn columns_align_by_display_width() {
        let rows = vec![
            row("日本", "1"),
            row("🍱 box", "22"),
            row("e\u{301}", "3"),
            row("👨\u{200d}👩\u{200d}👧", "4"),
        ];
        assert_eq!(
            render(&["name", "n"], &rows),
            "name   | n\n-------+---\n日本   | 1\n🍱 box | 22\ne\u{301}      | 3\n👨\u{200d}👩\u{200d}👧     | 4"
        );
    }

    #[test]
    fn a_wide_header_sets_the_column_width() {
        assert_eq!(
            render(&["名前", "x"], &[row("a", "b")]),
            "名前 | x\n-----+--\na    | b"
        );
    }

    #[test]
    fn a_failed_statement_note_is_worded_for_the_engine() {
        assert_eq!(
            error_note(0, &ExecuteError::StoppedAtDrainCap, Engine::Mysql),
            "error: statement 1: the statement was still running when the row limit was hit, \
             so it was stopped; a transactional write was rolled back, but a non-transactional \
             one may have kept part of its changes; use --read-only for plain reads \
             (session_closed)"
        );
        assert_eq!(
            error_note(2, &ExecuteError::Timeout, Engine::Sqlite),
            "error: statement 3: the statement timed out (timeout)"
        );
    }

    #[test]
    fn an_error_without_a_sqlstate_ends_with_its_code_alone() {
        assert_eq!(
            error_note(
                0,
                &ExecuteError::database("42P01", "relation \"x\" does not exist"),
                Engine::Postgres
            ),
            "error: statement 1: relation \"x\" does not exist (42P01)"
        );
    }

    #[test]
    fn unshown_result_sets_are_counted_in_words() {
        assert_eq!(more_results(0), None);
        assert_eq!(
            more_results(1).as_deref(),
            Some("1 more result set not shown")
        );
        assert_eq!(
            more_results(3).as_deref(),
            Some("3 more result sets not shown")
        );
    }
}
