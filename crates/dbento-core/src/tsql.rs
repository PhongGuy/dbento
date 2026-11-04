//! A T-SQL lexer that reads quotes, bracketed names and nesting comments as SQL Server does,
//! so the GO splitter and the SQL Server classifier never see a keyword inside text.

use std::ops::Range;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TsqlKind {
    Word,
    Str,
    Ident,
    LineComment,
    BlockComment,
    Space,
    Punct,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TsqlToken {
    pub(crate) kind: TsqlKind,
    pub(crate) range: Range<usize>,
}

/// The characters the server reads as a break between tokens, as measured against a live server.
fn is_space(c: char) -> bool {
    matches!(
        c,
        '\u{0}'..='\u{20}'
            | '\u{85}'
            | '\u{A0}'
            | '\u{1680}'
            | '\u{2000}'..='\u{200B}'
            | '\u{2028}'
            | '\u{2029}'
            | '\u{202F}'
            | '\u{205F}'
            | '\u{3000}'
    )
}

fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || matches!(c, '_' | '@' | '#' | '$')
        || (c >= '\u{80}' && !is_space(c))
}

/// The symbols that open a money literal, from the same sweep. Each is a token of its own, so
/// the number or keyword after it is seen; `$` only before a number, since `$action` is a word.
fn opens_money(c: char, next: Option<char>) -> bool {
    match c {
        '$' => next.is_some_and(|n| n.is_ascii_digit() || n == '.'),
        '\u{A2}'..='\u{A5}'
        | '\u{9F2}'
        | '\u{9F3}'
        | '\u{E3F}'
        | '\u{17DB}'
        | '\u{20A0}'..='\u{20B1}'
        | '\u{FDFC}'
        | '\u{FE69}'
        | '\u{FF04}'
        | '\u{FFE0}'
        | '\u{FFE1}'
        | '\u{FFE5}'
        | '\u{FFE6}' => true,
        _ => false,
    }
}

/// The index after the number opening at `i`; the server ends it there and starts a new token,
/// so `1SET` is two. `0x` takes hex digits; otherwise digits and an exponent whose digits may
/// be missing, so `1eCOMMIT` is two as well.
fn number_end(b: &[u8], i: usize) -> usize {
    let digits = |from: usize, keep: fn(&u8) -> bool| {
        from + b[from..].iter().take_while(|c| keep(c)).count()
    };
    if b[i] == b'0' && matches!(b.get(i + 1), Some(b'x' | b'X')) {
        return digits(i + 2, u8::is_ascii_hexdigit);
    }
    let end = digits(i, u8::is_ascii_digit);
    if matches!(b.get(end), Some(b'e' | b'E')) {
        digits(end + 1, u8::is_ascii_digit)
    } else {
        end
    }
}

/// Contiguous tokens covering all of `sql`; an unterminated token runs to the end.
pub(crate) fn tsql_tokens(sql: &str) -> Vec<TsqlToken> {
    let b = sql.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while let Some(lead) = sql[i..].chars().next() {
        let after = sql[i + lead.len_utf8()..].chars().next();
        let (kind, end) = match b[i] {
            _ if is_space(lead) => (TsqlKind::Space, run(sql, i, is_space)),
            _ if opens_money(lead, after) => (TsqlKind::Punct, i + lead.len_utf8()),
            c if c.is_ascii_digit() => (TsqlKind::Word, number_end(b, i)),
            b'\'' => (TsqlKind::Str, quote_end(b, i, b'\'')),
            b'[' => (TsqlKind::Ident, quote_end(b, i, b']')),
            b'"' => (TsqlKind::Ident, quote_end(b, i, b'"')),
            b'-' if b.get(i + 1) == Some(&b'-') => (
                TsqlKind::LineComment,
                run(sql, i, |c| !matches!(c, '\n' | '\r')),
            ),
            b'/' if b.get(i + 1) == Some(&b'*') => {
                (TsqlKind::BlockComment, block_comment_end(b, i))
            }
            c if is_word_char(lead) => {
                let end = run(sql, i, is_word_char);
                // A lone `N` touching a quote is the Unicode string prefix, part of the string.
                if end == i + 1 && c.eq_ignore_ascii_case(&b'n') && b.get(end) == Some(&b'\'') {
                    (TsqlKind::Str, quote_end(b, end, b'\''))
                } else {
                    (TsqlKind::Word, end)
                }
            }
            _ => (TsqlKind::Punct, i + 1),
        };
        out.push(TsqlToken {
            kind,
            range: i..end,
        });
        i = end;
    }
    out
}

fn run(sql: &str, start: usize, keep: impl Fn(char) -> bool) -> usize {
    sql[start..]
        .char_indices()
        .find(|&(_, c)| !keep(c))
        .map_or(sql.len(), |(n, _)| start + n)
}

/// The index after the quoted token opened at `open`; a doubled `close` stays inside.
fn quote_end(b: &[u8], open: usize, close: u8) -> usize {
    let mut j = open + 1;
    while j < b.len() {
        if b[j] != close {
            j += 1;
        } else if b.get(j + 1) == Some(&close) {
            j += 2;
        } else {
            return j + 1;
        }
    }
    b.len()
}

// Only `/*` and `*/` count inside a comment: quotes and `--` are text there, as the server reads it.
fn block_comment_end(b: &[u8], start: usize) -> usize {
    let (mut depth, mut j) = (0usize, start);
    while j + 1 < b.len() {
        match (b[j], b[j + 1]) {
            (b'/', b'*') => (depth, j) = (depth + 1, j + 2),
            (b'*', b'/') => {
                (depth, j) = (depth - 1, j + 2);
                if depth == 0 {
                    return j;
                }
            }
            _ => j += 1,
        }
    }
    b.len()
}

#[cfg(test)]
mod tests {
    use super::{TsqlKind, TsqlKind::*, tsql_tokens};

    fn toks(sql: &str) -> Vec<(TsqlKind, &str)> {
        tsql_tokens(sql)
            .into_iter()
            .map(|t| (t.kind, &sql[t.range]))
            .collect()
    }

    #[test]
    fn quotes_comments_and_names_lex_as_the_server_reads_them() {
        let sql = "SELECT N'a''b', [x]]y], \"q\"\"r\" FROM #t -- c\n/* a /* b */ c */@v";
        assert_eq!(
            toks(sql),
            [
                (Word, "SELECT"),
                (Space, " "),
                (Str, "N'a''b'"),
                (Punct, ","),
                (Space, " "),
                (Ident, "[x]]y]"),
                (Punct, ","),
                (Space, " "),
                (Ident, "\"q\"\"r\""),
                (Space, " "),
                (Word, "FROM"),
                (Space, " "),
                (Word, "#t"),
                (Space, " "),
                (LineComment, "-- c"),
                (Space, "\n"),
                (BlockComment, "/* a /* b */ c */"),
                (Word, "@v"),
            ]
        );
        assert_eq!(
            toks("AN'x' née a-b/c"),
            [
                (Word, "AN"),
                (Str, "'x'"),
                (Space, " "),
                (Word, "née"),
                (Space, " "),
                (Word, "a"),
                (Punct, "-"),
                (Word, "b"),
                (Punct, "/"),
                (Word, "c"),
            ]
        );
    }

    #[test]
    fn unterminated_tokens_run_to_the_end() {
        assert_eq!(
            toks("SELECT 'open"),
            [(Word, "SELECT"), (Space, " "), (Str, "'open")]
        );
        assert_eq!(toks("[open"), [(Ident, "[open")]);
        assert_eq!(
            toks("/* a /* b */ still"),
            [(BlockComment, "/* a /* b */ still")]
        );
        // Inside a comment only `/*` and `*/` count, so the quoted `/*` opens a second level.
        assert_eq!(
            toks("/* it's '/*' */ x"),
            [(BlockComment, "/* it's '/*' */ x")]
        );
    }
}
