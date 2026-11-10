//! PartiQL scripts, split into statements of tokens the way DynamoDB's PartiQL reads them.

use std::ops::Range;

/// What a token is: a bare word, a number, a `'string'`, a `"quoted name"` or one other character.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TokKind {
    Word,
    Number,
    Str,
    Ident,
    Punct,
}

/// One token and its byte range in the script.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Tok {
    pub(crate) kind: TokKind,
    pub(crate) range: Range<usize>,
}

/// One statement: from its first token's start to its last token's end, and its tokens.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Piece {
    pub(crate) range: Range<usize>,
    pub(crate) toks: Vec<Tok>,
}

/// A statement that does not lex: what is wrong, its byte offset from `range.start`, and the range.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SyntaxError {
    pub(crate) message: String,
    pub(crate) offset: usize,
    pub(crate) range: Range<usize>,
}

/// One result per statement, in order; a piece of only whitespace and comments yields none.
pub(crate) fn split(text: &str) -> Vec<Result<Piece, SyntaxError>> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut toks: Vec<Tok> = Vec::new();
    let mut i = 0;
    while let Some(c) = text[i..].chars().next() {
        let next = bytes.get(i + 1).copied();
        // Comments and whitespace emit nothing; an unclosed opener swallows the rest of the
        // script as one failed statement, so nothing after it is returned.
        let (kind, end) = match c {
            ';' => {
                flush(&mut toks, &mut out);
                i += 1;
                continue;
            }
            '-' if next == Some(b'-') => {
                i = text[i..].find('\n').map_or(text.len(), |n| i + n);
                continue;
            }
            '/' if next == Some(b'*') => match text[i + 2..].find("*/") {
                Some(n) => {
                    i += 2 + n + 2;
                    continue;
                }
                None => {
                    out.push(Err(unterminated(text, &toks, i, "block comment")));
                    return out;
                }
            },
            '\'' | '"' => match closing_quote(bytes, i) {
                Some(end) => (quoted_kind(c), end),
                None => {
                    let what = if c == '\'' { "string" } else { "quoted name" };
                    out.push(Err(unterminated(text, &toks, i, what)));
                    return out;
                }
            },
            c if c.is_whitespace() => {
                i += c.len_utf8();
                continue;
            }
            c if c.is_ascii_digit() => (TokKind::Number, run_end(text, i, |c| c == '.')),
            c if is_word_char(c) => (TokKind::Word, run_end(text, i, |_| false)),
            c => (TokKind::Punct, i + c.len_utf8()),
        };
        toks.push(Tok {
            kind,
            range: i..end,
        });
        i = end;
    }
    flush(&mut toks, &mut out);
    out
}

/// The range of every `split` result, parsed or not.
pub(crate) fn statement_ranges(text: &str) -> Vec<Range<usize>> {
    split(text)
        .into_iter()
        .map(|r| match r {
            Ok(p) => p.range,
            Err(e) => e.range,
        })
        .collect()
}

/// A name in double quotes, its own quotes doubled.
pub(crate) fn quote_ident(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

/// A string literal in single quotes, its own quotes doubled.
pub(crate) fn quote_string(text: &str) -> String {
    format!("'{}'", text.replace('\'', "''"))
}

/// Close the statement in progress, if it has any token.
fn flush(toks: &mut Vec<Tok>, out: &mut Vec<Result<Piece, SyntaxError>>) {
    if let (Some(first), Some(last)) = (toks.first(), toks.last()) {
        let range = first.range.start..last.range.end;
        out.push(Ok(Piece {
            range,
            toks: std::mem::take(toks),
        }));
    }
}

/// The error for an opener at `at` that never closes: it spans from the statement's first
/// token (or the opener itself) to the end of the script.
fn unterminated(text: &str, toks: &[Tok], at: usize, what: &str) -> SyntaxError {
    let start = toks.first().map_or(at, |t| t.range.start);
    SyntaxError {
        message: format!("unterminated {what}"),
        offset: at - start,
        range: start..text.len(),
    }
}

fn quoted_kind(quote: char) -> TokKind {
    if quote == '\'' {
        TokKind::Str
    } else {
        TokKind::Ident
    }
}

/// The end just past the quote closing the one at `open`, where a doubled quote is an escape.
fn closing_quote(bytes: &[u8], open: usize) -> Option<usize> {
    let quote = bytes[open];
    let mut j = open + 1;
    while j < bytes.len() {
        if bytes[j] == quote {
            if bytes.get(j + 1) == Some(&quote) {
                j += 2;
                continue;
            }
            return Some(j + 1);
        }
        j += 1;
    }
    None
}

fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_' || c == '$'
}

/// The end of the run starting at `start` over word characters and any `extra` ones, taken by
/// character so a non-ASCII name stays whole.
fn run_end(text: &str, start: usize, extra: impl Fn(char) -> bool) -> usize {
    text[start..]
        .char_indices()
        .find(|&(_, c)| !(is_word_char(c) || extra(c)))
        .map_or(text.len(), |(n, _)| start + n)
}

#[cfg(test)]
mod tests;
