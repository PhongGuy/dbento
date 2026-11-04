//! Lossless, engine-aware SQL lexer for the formatter.

use crate::Engine;
use crate::ranges::mysql_dashes_comment;
use crate::tsql::{TsqlKind, tsql_tokens};

/// What a token is, as far as layout and keyword case need to know.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Space,
    LineComment,
    BlockComment,
    Word,
    QuotedIdent,
    String,
    Number,
    Param,
    Punct,
    Op,
}

/// One token: its kind and its byte range in the source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Tok {
    pub(crate) kind: Kind,
    pub(crate) range: std::ops::Range<usize>,
}

/// Contiguous tokens covering all of `sql`; None on an unterminated string, quoted
/// identifier, dollar quote or block comment, and on MySQL text whose token boundaries
/// depend on the session (a backslash in a string, a `DELIMITER` line).
pub(crate) fn lex(engine: Engine, sql: &str) -> Option<Vec<Tok>> {
    lex_to(engine, sql, sql.len())
}

/// The tokens of `sql` that start before byte `upto`, so text past them need not lex;
/// None when one of those tokens is unterminated, or on MongoDB and SurrealDB, which are not
/// SQL, so the formatter leaves their text alone.
pub(crate) fn lex_to(engine: Engine, sql: &str, upto: usize) -> Option<Vec<Tok>> {
    match engine {
        Engine::Mongodb | Engine::Surrealdb => return None,
        Engine::Mssql => return mssql_toks(sql, upto),
        _ => {}
    }
    let mut toks = Vec::new();
    let mut i = 0;
    while i < upto.min(sql.len()) {
        let (kind, end) = next(engine, sql, i)?;
        if engine == Engine::Mysql && kind == Kind::Word && delimiter_line(sql, i, end) {
            return None;
        }
        toks.push(Tok {
            kind,
            range: i..end,
        });
        i = end;
    }
    Some(toks)
}

// `tsql_tokens` reads T-SQL as the server does and runs an unterminated token to the end, so
// each quote and comment is checked here, against the same rules, to tell that case apart.
fn mssql_toks(sql: &str, upto: usize) -> Option<Vec<Tok>> {
    let b = sql.as_bytes();
    tsql_tokens(sql)
        .into_iter()
        .take_while(|t| t.range.start < upto)
        .map(|t| {
            let start = t.range.start;
            let text = &sql[t.range.clone()];
            let kind = match t.kind {
                TsqlKind::Space => Kind::Space,
                TsqlKind::LineComment => Kind::LineComment,
                TsqlKind::BlockComment => {
                    block_comment(b, start, true)?;
                    Kind::BlockComment
                }
                TsqlKind::Str => {
                    // A leading `N` is part of the token, and the quote opens after it.
                    quoted(b, start + usize::from(b[start] != b'\''), b'\'', false)?;
                    Kind::String
                }
                TsqlKind::Ident => {
                    let close = if b[start] == b'[' { b']' } else { b[start] };
                    quoted(b, start, close, false)?;
                    Kind::QuotedIdent
                }
                TsqlKind::Word if b[start].is_ascii_digit() => Kind::Number,
                TsqlKind::Word if b[start] == b'@' => Kind::Param,
                TsqlKind::Word => Kind::Word,
                TsqlKind::Punct if matches!(text, "(" | ")" | "," | ";" | ".") => Kind::Punct,
                TsqlKind::Punct => Kind::Op,
            };
            Some(Tok {
                kind,
                range: t.range,
            })
        })
        .collect()
}

// The mysql client reads a line opening with `DELIMITER` as a client command that moves
// statement boundaries, so text holding one is not SQL the formatter can lay out.
fn delimiter_line(sql: &str, i: usize, end: usize) -> bool {
    let b = sql.as_bytes();
    let line_start = sql[..i].rfind('\n').map_or(0, |n| n + 1);
    sql[i..end].eq_ignore_ascii_case("delimiter")
        && b.get(end).is_none_or(u8::is_ascii_whitespace)
        && b[line_start..i].iter().all(u8::is_ascii_whitespace)
}

// DuckDB's parser is libpg_query, so its SQL tokenizes exactly as Postgres text does.
fn pg_like(engine: Engine) -> bool {
    matches!(engine, Engine::Postgres | Engine::Duckdb)
}

// Every token ends on an ASCII byte or at the end of input, and non-ASCII bytes are
// always inside a word, quote or comment, so byte offsets stay on char boundaries.
fn next(engine: Engine, sql: &str, i: usize) -> Option<(Kind, usize)> {
    let b = sql.as_bytes();
    let pg = pg_like(engine);
    let my = engine == Engine::Mysql;
    let at = |j: usize| b.get(j).copied().unwrap_or(0);
    let line_end = || sql[i..].find('\n').map_or(b.len(), |n| i + n);
    let c = b[i];
    Some(match c {
        _ if c.is_ascii_whitespace() => (Kind::Space, run(b, i, |c| c.is_ascii_whitespace())),
        b'-' if at(i + 1) == b'-' && (!my || mysql_dashes_comment(b.get(i + 2))) => {
            (Kind::LineComment, line_end())
        }
        b'#' if my => (Kind::LineComment, line_end()),
        b'/' if at(i + 1) == b'*' => (Kind::BlockComment, block_comment(b, i, pg)?),
        b'\'' | b'"' if my => (Kind::String, mysql_string(b, i)?),
        b'\'' => (Kind::String, quoted(b, i, b'\'', false)?),
        b'"' => (Kind::QuotedIdent, quoted(b, i, b'"', false)?),
        b'`' if !pg => (Kind::QuotedIdent, quoted(b, i, b'`', false)?),
        b'[' if engine == Engine::Sqlite => {
            let end = b[i..].iter().position(|&c| c == b']')?;
            (Kind::QuotedIdent, i + end + 1)
        }
        b'(' | b')' | b',' | b';' => (Kind::Punct, i + 1),
        b'[' | b']' if engine != Engine::Sqlite => (Kind::Punct, i + 1),
        b'.' if !at(i + 1).is_ascii_digit() => (Kind::Punct, i + 1),
        _ if c.is_ascii_digit() || c == b'.' => (Kind::Number, number(b, i)),
        b'$' if pg && at(i + 1).is_ascii_digit() => {
            (Kind::Param, run(b, i + 1, |c| c.is_ascii_digit()))
        }
        b'$' if pg => match dollar_quote(sql, i) {
            Some(end) => (Kind::String, end?),
            None => (Kind::Op, i + 1),
        },
        b'?' if !pg => (Kind::Param, run(b, i + 1, |c| c.is_ascii_digit())),
        b'@' if my && at(i + 1) == b'@' => (Kind::Param, run(b, i + 2, is_word_char)),
        b'@' if my && matches!(at(i + 1), b'\'' | b'"') => (Kind::Param, mysql_string(b, i + 1)?),
        b'@' if my && at(i + 1) == b'`' => (Kind::Param, quoted(b, i + 1, b'`', false)?),
        b':' | b'@' | b'$' if !pg && (!my || c == b'@') && is_word_char(at(i + 1)) => {
            (Kind::Param, run(b, i + 1, is_word_char))
        }
        _ if is_word_start(c) => word_or_prefixed(b, i, engine)?,
        _ if is_op_byte(c, engine) => (Kind::Op, op_run(b, i, engine)),
        _ => (Kind::Op, i + 1),
    })
}

fn is_word_start(c: u8) -> bool {
    c.is_ascii_alphabetic() || c == b'_' || c >= 0x80
}

fn is_word_char(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'$' || c >= 0x80
}

/// End of the run starting at `i` whose bytes all satisfy `keep`.
fn run(b: &[u8], i: usize, keep: impl Fn(u8) -> bool) -> usize {
    i + b[i..].iter().take_while(|&&c| keep(c)).count()
}

/// End of the quoted run opened at `i`; a doubled `close` is an escape, and with
/// `backslash` a backslash escapes the next byte.
fn quoted(b: &[u8], i: usize, close: u8, backslash: bool) -> Option<usize> {
    let mut j = i + 1;
    while j < b.len() {
        match b[j] {
            b'\\' if backslash => j += 2,
            c if c == close && b.get(j + 1) == Some(&close) => j += 2,
            c if c == close => return Some(j + 1),
            _ => j += 1,
        }
    }
    None
}

/// End of a MySQL `'` or `"` string opened at `i`; None when it holds a backslash, since
/// whether that escapes the quote depends on the session's `sql_mode`.
fn mysql_string(b: &[u8], i: usize) -> Option<usize> {
    let end = quoted(b, i, b[i], false)?;
    (!b[i..end].contains(&b'\\')).then_some(end)
}

fn block_comment(b: &[u8], i: usize, nest: bool) -> Option<usize> {
    let (mut j, mut depth) = (i + 2, 1);
    while j + 1 < b.len() {
        match (b[j], b[j + 1]) {
            (b'*', b'/') => {
                depth -= 1;
                j += 2;
                if depth == 0 {
                    return Some(j);
                }
            }
            (b'/', b'*') if nest => {
                depth += 1;
                j += 2;
            }
            _ => j += 1,
        }
    }
    None
}

// Alphanumerics, `_` and `.` continue a number; a sign continues it only right after an exponent `e`.
fn number(b: &[u8], i: usize) -> usize {
    let mut j = i + 1;
    while let Some(&c) = b.get(j) {
        let sign = matches!(c, b'+' | b'-') && matches!(b[j - 1], b'e' | b'E');
        if !(c.is_ascii_alphanumeric() || c == b'_' || c == b'.' || sign) {
            break;
        }
        j += 1;
    }
    j
}

/// None when `$` at `i` opens no dollar quote; Some(None) when the quote is unterminated.
fn dollar_quote(sql: &str, i: usize) -> Option<Option<usize>> {
    let b = sql.as_bytes();
    let tag_end = match b.get(i + 1) {
        Some(&c) if is_word_start(c) => run(b, i + 1, |c| c != b'$' && is_word_char(c)),
        _ => i + 1,
    };
    if b.get(tag_end) != Some(&b'$') {
        return None;
    }
    let delim = &sql[i..=tag_end];
    let body = tag_end + 1;
    Some(sql[body..].find(delim).map(|n| body + n + delim.len()))
}

// A Postgres `e`, `b`, `x`, `n` or `U&` prefix (MySQL: `b`, `x`, `n`; SQLite: `x`) touching
// a quote is part of that literal; splitting them would let the layout insert a space.
fn word_or_prefixed(b: &[u8], i: usize, engine: Engine) -> Option<(Kind, usize)> {
    let pg = pg_like(engine);
    let end = run(b, i, is_word_char);
    let word = b[i..end].to_ascii_lowercase();
    let next = b.get(end).copied();
    let prefix = match engine {
        Engine::Postgres | Engine::Duckdb => &[&b"e"[..], b"b", b"x", b"n"][..],
        Engine::Mysql => &[&b"b"[..], b"x", b"n"][..],
        _ => &[&b"x"[..]][..],
    };
    if next == Some(b'\'') && prefix.contains(&&word[..]) {
        if engine == Engine::Mysql {
            return Some((Kind::String, mysql_string(b, end)?));
        }
        return Some((Kind::String, quoted(b, end, b'\'', pg && word == b"e")?));
    }
    if pg && word == b"u" && next == Some(b'&') {
        match b.get(end + 1) {
            Some(b'\'') => return Some((Kind::String, quoted(b, end + 1, b'\'', false)?)),
            Some(b'"') => return Some((Kind::QuotedIdent, quoted(b, end + 1, b'"', false)?)),
            _ => {}
        }
    }
    Some((Kind::Word, end))
}

// Operator bytes are the ASCII punctuation not claimed by quotes, punct, comments or `$`.
fn is_op_byte(c: u8, engine: Engine) -> bool {
    let claimed: &[u8] = match engine {
        Engine::Postgres | Engine::Duckdb => b"'\"(),;._$[]",
        Engine::Mysql => b"'\"(),;._$`[]#",
        _ => b"'\"(),;._$`[",
    };
    c.is_ascii_punctuation() && !claimed.contains(&c)
}

// An operator run stops where a comment starts, and off Postgres where a parameter starts.
fn op_run(b: &[u8], i: usize, engine: Engine) -> usize {
    let my = engine == Engine::Mysql;
    let mut j = i + 1;
    while let Some(&c) = b.get(j) {
        let next = b.get(j + 1).copied().unwrap_or(0);
        let dashes = c == b'-' && next == b'-' && (!my || mysql_dashes_comment(b.get(j + 2)));
        let comment = dashes || (c == b'/' && next == b'*');
        let param = match engine {
            Engine::Postgres | Engine::Duckdb => false,
            Engine::Mysql => {
                c == b'?' || (c == b'@' && (is_word_char(next) || b"@'\"`".contains(&next)))
            }
            _ => c == b'?' || (matches!(c, b':' | b'@') && is_word_char(next)),
        };
        if !is_op_byte(c, engine) || comment || param {
            break;
        }
        j += 1;
    }
    j
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Engine::{Duckdb, Mysql, Postgres, Sqlite};

    fn toks(engine: Engine, sql: &str) -> Vec<(Kind, &str)> {
        let t = lex(engine, sql).expect("lexes");
        assert_eq!(
            t.iter().map(|t| &sql[t.range.clone()]).collect::<String>(),
            sql,
            "lossless"
        );
        t.iter()
            .filter(|t| t.kind != Kind::Space)
            .map(|t| (t.kind, &sql[t.range.clone()]))
            .collect()
    }

    #[test]
    fn every_byte_is_covered_in_order() {
        for sql in [
            "select a, b from t -- c\n where x = 'y''z';",
            "  ",
            "",
            "SELECT\t\"a\"\"b\"\r\n",
        ] {
            for e in [Postgres, Sqlite, Mysql] {
                toks(e, sql);
            }
        }
    }

    #[test]
    fn dollar_quotes_are_one_string_and_dollar_digits_a_param() {
        use Kind::*;
        assert_eq!(
            toks(Postgres, "select $$it's$$, $tag$ x $$ y $tag$, $1"),
            [
                (Word, "select"),
                (String, "$$it's$$"),
                (Punct, ","),
                (String, "$tag$ x $$ y $tag$"),
                (Punct, ","),
                (Param, "$1")
            ]
        );
    }

    #[test]
    fn prefix_strings_are_one_token() {
        use Kind::*;
        assert_eq!(
            toks(Postgres, r"e'it\'s' B'101' x'ff' U&'d\0061' n'x'"),
            [
                (String, r"e'it\'s'"),
                (String, "B'101'"),
                (String, "x'ff'"),
                (String, r"U&'d\0061'"),
                (String, "n'x'")
            ]
        );
        assert_eq!(toks(Sqlite, "x'ff'"), [(String, "x'ff'")]);
        assert_eq!(
            toks(Postgres, "date '2024-01-01'"),
            [(Word, "date"), (String, "'2024-01-01'")]
        );
    }

    #[test]
    fn casts_and_operators() {
        use Kind::*;
        assert_eq!(
            toks(Postgres, "a::int, b->>'k', arr[1]"),
            [
                (Word, "a"),
                (Op, "::"),
                (Word, "int"),
                (Punct, ","),
                (Word, "b"),
                (Op, "->>"),
                (String, "'k'"),
                (Punct, ","),
                (Word, "arr"),
                (Punct, "["),
                (Number, "1"),
                (Punct, "]")
            ]
        );
        assert_eq!(
            toks(Postgres, "a-- c"),
            [(Word, "a"), (LineComment, "-- c")]
        );
        assert_eq!(
            toks(Postgres, "1.5e-3 .5"),
            [(Number, "1.5e-3"), (Number, ".5")]
        );
    }

    #[test]
    fn comments_nest_only_on_postgres() {
        use Kind::*;
        assert_eq!(
            toks(Postgres, "/* a /* b */ c */ x"),
            [(BlockComment, "/* a /* b */ c */"), (Word, "x")]
        );
        assert_eq!(
            toks(Sqlite, "/* a /* b */ x"),
            [(BlockComment, "/* a /* b */"), (Word, "x")]
        );
        assert_eq!(
            toks(Sqlite, "-- c\nx"),
            [(LineComment, "-- c"), (Word, "x")]
        );
    }

    #[test]
    fn sqlite_quotes_and_params() {
        use Kind::*;
        assert_eq!(
            toks(Sqlite, "`a b` [c d] \"e\" ? ?1 :n @v $p"),
            [
                (QuotedIdent, "`a b`"),
                (QuotedIdent, "[c d]"),
                (QuotedIdent, "\"e\""),
                (Param, "?"),
                (Param, "?1"),
                (Param, ":n"),
                (Param, "@v"),
                (Param, "$p")
            ]
        );
    }

    #[test]
    fn unterminated_input_does_not_lex() {
        for (e, sql) in [
            (Postgres, "select 'abc"),
            (Postgres, "select $$ abc"),
            (Postgres, "/* a /* b */"),
            (Sqlite, "select [a"),
            (Sqlite, "select `a"),
            (Sqlite, "select \"a"),
        ] {
            assert_eq!(lex(e, sql), None, "{sql}");
        }
    }

    #[test]
    fn mysql_comments_quotes_and_prefixes() {
        use Kind::*;
        assert_eq!(
            toks(
                Mysql,
                "select `a``b`, \"s\", 'it''s', x'ff', b'01', N'x' # c\n-- d\n--e"
            ),
            [
                (Word, "select"),
                (QuotedIdent, "`a``b`"),
                (Punct, ","),
                (String, "\"s\""),
                (Punct, ","),
                (String, "'it''s'"),
                (Punct, ","),
                (String, "x'ff'"),
                (Punct, ","),
                (String, "b'01'"),
                (Punct, ","),
                (String, "N'x'"),
                (LineComment, "# c"),
                (LineComment, "-- d"),
                (Op, "--"),
                (Word, "e")
            ]
        );
    }

    #[test]
    fn mysql_versioned_comments_variables_and_brackets() {
        use Kind::*;
        assert_eq!(
            toks(
                Mysql,
                "/*!40101 SET NAMES utf8 */; /*+ BKA(t) */ select @@sql_mode, @v, @'q', ?, a[1]"
            ),
            [
                (BlockComment, "/*!40101 SET NAMES utf8 */"),
                (Punct, ";"),
                (BlockComment, "/*+ BKA(t) */"),
                (Word, "select"),
                (Param, "@@sql_mode"),
                (Punct, ","),
                (Param, "@v"),
                (Punct, ","),
                (Param, "@'q'"),
                (Punct, ","),
                (Param, "?"),
                (Punct, ","),
                (Word, "a"),
                (Punct, "["),
                (Number, "1"),
                (Punct, "]")
            ]
        );
        assert_eq!(
            toks(Mysql, "/* a /* b */ x"),
            [(BlockComment, "/* a /* b */"), (Word, "x")]
        );
    }

    #[test]
    fn mysql_text_whose_boundaries_depend_on_the_session_does_not_lex() {
        for sql in [
            "select 'a\\'b'",
            "select \"a\\\\\"",
            "DELIMITER //\nselect 1 //",
            "select 1;\n  delimiter ;",
            "select `a",
            "select 'a",
            "/* open",
        ] {
            assert_eq!(lex(Mysql, sql), None, "{sql}");
        }
        assert!(lex(Mysql, "select delimiter_x, `delimiter` from t").is_some());
    }

    #[test]
    fn duckdb_lexes_with_the_postgres_arms() {
        for sql in [
            "select $$it's$$, $tag$ x $$ y $tag$, $1",
            r"e'it\'s' B'101' x'ff' U&'d\0061' n'x'",
            "/* a /* b */ c */ select 1",
            "select a::int[] from t where b ? 'k' and c[1] = 2",
            "select \"Q\"\"x\", `b` from t -- c",
        ] {
            assert_eq!(lex(Duckdb, sql), lex(Postgres, sql), "{sql}");
        }
    }
}
