//! Statement ranges: where each statement a `Script` runs sits in the script's text, so a
//! shell can run the statement under the caret without rewriting the user's SQL.

use std::ops::Range;

use crate::Engine;
use crate::split::split;
use crate::tsql::{TsqlKind, TsqlToken, tsql_tokens};

/// Byte ranges of the statements `Script` runs on `sql`, in order and without overlap. Each
/// range ends after its `;` (before it on MySQL, or at the end of the text), and its trimmed text equals that
/// statement's `Pending::sql()`. Blank pieces are omitted. On SQLite, empty `;` statements
/// and blank text join the statement after them; a lexical error makes the rest one range.
/// On Postgres, a lexical error makes the whole text one range. On MySQL a range stops before
/// its delimiter, `DELIMITER` lines belong to no range, and an unterminated quote makes the
/// rest one range. On SQL Server a range is a batch ending before its GO line, GO lines
/// belong to no range, and an unterminated string, name or comment makes the rest one batch.
/// On MongoDB a range starts at the statement's first token and ends after its `;` or last token.
/// On SurrealDB a range is a unit, which may hold several statements.
/// On Redis a range is one command line, without its comments and blank lines.
pub fn statement_ranges(engine: Engine, sql: &str) -> Vec<Range<usize>> {
    match engine {
        Engine::Postgres => split(sql)
            .into_iter()
            .filter(|p| !p.blank)
            .map(|p| p.range)
            .collect(),
        Engine::Sqlite => sqlite_ranges(sql),
        Engine::Mysql => mysql_ranges(sql),
        Engine::Mssql => mssql_ranges(sql).into_iter().map(|b| b.range).collect(),
        // DuckDB's parser is libpg_query's, so its statement boundaries are Postgres's.
        Engine::Duckdb => statement_ranges(Engine::Postgres, sql),
        Engine::Mongodb => crate::mongo::split::statement_ranges(sql),
        Engine::Surrealdb => crate::surreal::units::statement_ranges(sql),
        Engine::Redis => crate::redis::lex::statement_ranges(sql),
        Engine::Dynamodb => crate::dynamo::lex::statement_ranges(sql),
    }
}

/// Top-level statements in `sql`: counted by the lexer on SurrealDB, where a unit holds several;
/// the batch count on SQL Server and the range count elsewhere. MCP's one-statement gate reads this.
pub fn statement_count(engine: Engine, sql: &str) -> usize {
    match engine {
        Engine::Surrealdb => crate::surreal::units::statement_count(sql),
        Engine::Postgres
        | Engine::Sqlite
        | Engine::Mysql
        | Engine::Mssql
        | Engine::Duckdb
        | Engine::Mongodb
        | Engine::Redis
        | Engine::Dynamodb => statement_ranges(engine, sql).len(),
    }
}

/// The range each result of `Script` takes its text from, in result order: on SurrealDB a
/// unit's range once per statement it holds; elsewhere `statement_ranges`.
pub fn result_ranges(engine: Engine, sql: &str) -> Vec<Range<usize>> {
    match engine {
        Engine::Surrealdb => crate::surreal::units::result_ranges(sql),
        Engine::Postgres
        | Engine::Sqlite
        | Engine::Mysql
        | Engine::Mssql
        | Engine::Duckdb
        | Engine::Mongodb
        | Engine::Redis
        | Engine::Dynamodb => statement_ranges(engine, sql),
    }
}

/// One SQL Server batch: its text and how many times its GO line asks for it to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MssqlBatch {
    pub(crate) range: Range<usize>,
    pub(crate) repeat: u32,
}

/// Splits a T-SQL script at its GO lines, which belong to no batch. A batch of only
/// whitespace and comments is omitted with its count; `repeat` is 1 when GO has no count.
pub(crate) fn mssql_ranges(sql: &str) -> Vec<MssqlBatch> {
    let toks = tsql_tokens(sql);
    let mut out = Vec::new();
    let (mut start, mut code, mut skip_to) = (0, false, 0);
    for (i, t) in toks.iter().enumerate() {
        if t.range.start < skip_to {
            continue;
        }
        if let Some((line, end, repeat)) = go_line(sql, &toks, i) {
            if code {
                out.push(MssqlBatch {
                    range: start..line,
                    repeat,
                });
            }
            (start, code, skip_to) = (end, false, end);
        } else if !matches!(
            t.kind,
            TsqlKind::Space | TsqlKind::LineComment | TsqlKind::BlockComment
        ) {
            code = true;
        }
    }
    if code {
        out.push(MssqlBatch {
            range: start..sql.len(),
            repeat: 1,
        });
    }
    out
}

/// When token `i` opens a GO line, its start, the index after its newline, and its count.
/// GO leads its line after spaces or tabs, then come only an optional count, an optional
/// `--` comment and the newline (a `\r` may precede it) or the end of the text.
fn go_line(sql: &str, toks: &[TsqlToken], i: usize) -> Option<(usize, usize, u32)> {
    let blank = |s: &str| s.bytes().all(|c| c == b' ' || c == b'\t');
    if toks[i].kind != TsqlKind::Word || !sql[toks[i].range.clone()].eq_ignore_ascii_case("go") {
        return None;
    }
    let line = match i.checked_sub(1).map(|p| &toks[p]) {
        None => 0,
        Some(p) if p.kind == TsqlKind::Space => {
            let text = &sql[p.range.clone()];
            let tail = text.rfind('\n').map_or(0, |n| n + 1);
            if !blank(&text[tail..]) || (tail == 0 && i > 1) {
                return None;
            }
            p.range.start + tail
        }
        Some(_) => return None,
    };
    let (mut repeat, mut comment) = (None, false);
    for t in &toks[i + 1..] {
        let text = &sql[t.range.clone()];
        match t.kind {
            TsqlKind::Space => {
                if let Some(nl) = text.find('\n') {
                    let before = &text[..nl];
                    return blank(before.strip_suffix('\r').unwrap_or(before)).then_some((
                        line,
                        t.range.start + nl + 1,
                        repeat.unwrap_or(1),
                    ));
                }
                if !blank(text) {
                    return None;
                }
            }
            TsqlKind::Word if repeat.is_none() && !comment => {
                let n = text
                    .bytes()
                    .all(|c| c.is_ascii_digit())
                    .then(|| text.parse::<u32>());
                repeat = Some(n.and_then(Result::ok).filter(|&n| n >= 1)?);
            }
            TsqlKind::LineComment if !comment => comment = true,
            TsqlKind::Word
            | TsqlKind::Str
            | TsqlKind::Ident
            | TsqlKind::LineComment
            | TsqlKind::BlockComment
            | TsqlKind::Punct => return None,
        }
    }
    Some((line, sql.len(), repeat.unwrap_or(1)))
}

// State machine from SQLite's complete.c (public domain).

#[derive(Clone, Copy, PartialEq)]
enum Tk {
    Semi,
    Ws,
    Other,
    Explain,
    Create,
    Temp,
    Trigger,
    End,
}

/// `TRANS[state][token]`; a statement is complete when a `;` lands in state 1. States 5
/// to 7 are inside a trigger body, where `;` only ends the statement after `END`.
const TRANS: [[u8; 8]; 8] = [
    [1, 0, 2, 3, 4, 2, 2, 2],
    [1, 1, 2, 3, 4, 2, 2, 2],
    [1, 2, 2, 2, 2, 2, 2, 2],
    [1, 3, 3, 2, 4, 2, 2, 2],
    [1, 4, 2, 2, 2, 4, 5, 2],
    [6, 5, 5, 5, 5, 5, 5, 5],
    [6, 6, 5, 5, 5, 5, 5, 7],
    [1, 7, 5, 5, 5, 5, 5, 5],
];

fn is_id_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$' || b >= 0x80
}

fn keyword(word: &[u8]) -> Tk {
    let is = |k: &str| word.eq_ignore_ascii_case(k.as_bytes());
    if is("create") {
        Tk::Create
    } else if is("temp") || is("temporary") {
        Tk::Temp
    } else if is("trigger") {
        Tk::Trigger
    } else if is("end") {
        Tk::End
    } else if is("explain") {
        Tk::Explain
    } else {
        Tk::Other
    }
}

/// Splits as SQLite's prepare does: a piece holding only whitespace, comments and `;`
/// stays pending and becomes the head of the next statement, as SQLite reports its text.
fn sqlite_ranges(sql: &str) -> Vec<Range<usize>> {
    let b = sql.as_bytes();
    let find = |from: usize, pat: &[u8]| {
        b.get(from..)
            .and_then(|rest| rest.windows(pat.len()).position(|w| w == pat))
            .map(|p| from + p)
    };
    let (mut out, mut start, mut blank, mut state, mut i) = (Vec::new(), 0, true, 0u8, 0);
    while i < b.len() {
        let (token, next) = match b[i] {
            b';' => (Tk::Semi, i + 1),
            b' ' | b'\t' | b'\n' | b'\r' | b'\x0c' => (Tk::Ws, i + 1),
            // An unterminated block comment runs to the end, as SQLite's tokenizer reads it.
            b'/' if b.get(i + 1) == Some(&b'*') => {
                (Tk::Ws, find(i + 2, b"*/").map_or(b.len(), |e| e + 2))
            }
            b'-' if b.get(i + 1) == Some(&b'-') => {
                (Tk::Ws, find(i + 2, b"\n").map_or(b.len(), |e| e + 1))
            }
            // A doubled quote reads as two adjacent strings, which classifies the same.
            q @ (b'\'' | b'"' | b'`' | b'[') => {
                let close: &[u8] = if q == b'[' { b"]" } else { &b[i..=i] };
                match find(i + 1, close) {
                    Some(e) => (Tk::Other, e + 1),
                    // A lexical error: the rest of the text is one statement, as `Script` runs it.
                    None => {
                        out.push(start..b.len());
                        return out;
                    }
                }
            }
            c if is_id_byte(c) => {
                let end = (i..b.len()).find(|&j| !is_id_byte(b[j])).unwrap_or(b.len());
                (keyword(&b[i..end]), end)
            }
            _ => (Tk::Other, i + 1),
        };
        state = TRANS[usize::from(state)][token as usize];
        if token == Tk::Semi && state == 1 {
            if !blank {
                out.push(start..next);
                start = next;
                blank = true;
            }
        } else if token != Tk::Ws && token != Tk::Semi {
            blank = false;
        }
        i = next;
    }
    if !blank {
        out.push(start..b.len());
    }
    out
}

/// Whether `--` followed by `next` opens a MySQL comment: only whitespace, a control
/// character or the end of the text may follow the dashes.
pub(crate) fn mysql_dashes_comment(next: Option<&u8>) -> bool {
    next.is_none_or(|c| c.is_ascii_whitespace() || c.is_ascii_control())
}

/// Splits as the `mysql` client does: ranges end before the delimiter, `DELIMITER` lines
/// switch it and belong to no range, and `/*!` or `/*+` comments are code never split inside.
fn mysql_ranges(sql: &str) -> Vec<Range<usize>> {
    let b = sql.as_bytes();
    let find = |from: usize, pat: &[u8]| {
        b.get(from..)
            .and_then(|rest| rest.windows(pat.len()).position(|w| w == pat))
            .map(|p| from + p)
    };
    let line_end = |from: usize| find(from, b"\n").unwrap_or(b.len());
    let mut delim: &[u8] = b";";
    let (mut out, mut start, mut code, mut i) = (Vec::new(), 0, false, 0);
    while i < b.len() {
        // A DELIMITER line counts only before the pending statement has any code. As in the
        // mysql client, its first token is the delimiter and the rest of the line is ignored.
        if !code
            && b.len() >= i + 9
            && b[i..i + 9].eq_ignore_ascii_case(b"delimiter")
            && b.get(i + 9).is_none_or(u8::is_ascii_whitespace)
        {
            let end = line_end(i);
            if let Some(arg) = sql[i + 9..end].split_ascii_whitespace().next() {
                delim = arg.as_bytes();
            }
            i = (end + 1).min(b.len());
            start = i;
            continue;
        }
        if b[i..].starts_with(delim) {
            if code {
                out.push(start..i);
            }
            i += delim.len();
            start = i;
            code = false;
            continue;
        }
        i = match b[i] {
            b'#' => line_end(i),
            b'-' if b.get(i + 1) == Some(&b'-') && mysql_dashes_comment(b.get(i + 2)) => {
                line_end(i)
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                // Executable and hint comments are code; an unterminated comment runs to the end.
                code |= matches!(b.get(i + 2), Some(b'!' | b'+'));
                find(i + 2, b"*/").map_or(b.len(), |e| e + 2)
            }
            q @ (b'\'' | b'"' | b'`') => {
                code = true;
                match quote_end(b, i, q) {
                    Some(e) => e,
                    // An unterminated quote makes the rest one statement.
                    None => {
                        out.push(start..b.len());
                        return out;
                    }
                }
            }
            c if c.is_ascii_whitespace() => i + 1,
            _ => {
                code = true;
                i + 1
            }
        };
    }
    if code {
        out.push(start..b.len());
    }
    out
}

/// The index after the quote opened at `open`. A doubled quote stays inside; in strings, but
/// not in backtick names, a backslash escapes the byte after it.
fn quote_end(b: &[u8], open: usize, q: u8) -> Option<usize> {
    let mut j = open + 1;
    while j < b.len() {
        match b[j] {
            b'\\' if q != b'`' => j += 2,
            c if c == q && b.get(j + 1) == Some(&q) => j += 2,
            c if c == q => return Some(j + 1),
            _ => j += 1,
        }
    }
    None
}

#[cfg(test)]
mod mssql_tests {
    use super::mssql_ranges;

    #[test]
    fn a_go_count_repeats_its_batch_and_a_blank_batch_drops_its_count() {
        let sql = "SELECT 1\ngo 3 -- three\nSELECT 2\nGO\n\nGO 5\nSELECT 3";
        let got: Vec<(&str, u32)> = mssql_ranges(sql)
            .into_iter()
            .map(|b| (sql[b.range.clone()].trim(), b.repeat))
            .collect();
        assert_eq!(got, [("SELECT 1", 3), ("SELECT 2", 1), ("SELECT 3", 1)]);
        let repeats = |s: &str| mssql_ranges(s).iter().map(|b| b.repeat).collect::<Vec<_>>();
        assert_eq!(repeats("SELECT 1\nGO 2"), [2]);
        assert_eq!(repeats("SELECT 1"), [1]);
    }
}
