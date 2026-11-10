//! The shape of one PartiQL statement: what it does, which table it names and which attributes
//! it projects, read from its tokens without a full parser.

use std::ops::Range;

use super::lex::{Piece, SyntaxError, Tok, TokKind, split};

/// What a statement does, from its first keyword.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    Select,
    Insert,
    Update,
    Delete,
    Other,
}

/// The table a statement names, and the index after a `.` if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Target {
    pub(crate) table: String,
    pub(crate) index: Option<String>,
}

/// One statement's range, kind, target and plain-name projection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PartiqlStatement {
    pub(crate) range: Range<usize>,
    pub(crate) kind: Kind,
    pub(crate) target: Option<Target>,
    pub(crate) projection: Option<Vec<String>>,
}

/// One result per statement of `text`, in order; a statement that does not lex keeps its error.
pub(crate) fn parse_script(text: &str) -> Vec<Result<PartiqlStatement, SyntaxError>> {
    split(text)
        .into_iter()
        .map(|r| r.map(|piece| shape(text, piece)))
        .collect()
}

fn shape(text: &str, piece: Piece) -> PartiqlStatement {
    let toks = piece.toks.as_slice();
    let word = |i: usize, w: &str| toks.get(i).is_some_and(|t| is_word(text, t, w));
    let kind = if word(0, "SELECT") {
        Kind::Select
    } else if word(0, "INSERT") {
        Kind::Insert
    } else if word(0, "UPDATE") {
        Kind::Update
    } else if word(0, "DELETE") {
        Kind::Delete
    } else {
        Kind::Other
    };
    // A select's projection ends and its target begins at the first top-level FROM; the other
    // kinds name their table at a fixed place after their opening keywords.
    let from = match kind {
        Kind::Select => top_level_from(text, toks),
        Kind::Insert | Kind::Update | Kind::Delete | Kind::Other => None,
    };
    let target_at = match kind {
        Kind::Select => from.map(|f| f + 1),
        Kind::Insert => word(1, "INTO").then_some(2),
        Kind::Update => Some(1),
        Kind::Delete => word(1, "FROM").then_some(2),
        Kind::Other => None,
    };
    PartiqlStatement {
        range: piece.range,
        kind,
        target: target_at.and_then(|at| target(text, &toks[at.min(toks.len())..])),
        projection: from.and_then(|f| projection(text, &toks[1..f])),
    }
}

/// The index of the first `FROM` outside any bracket; only punctuation moves the depth, so a
/// bracket inside a string or quoted name never counts.
fn top_level_from(text: &str, toks: &[Tok]) -> Option<usize> {
    let mut depth = 0usize;
    for (i, tok) in toks.iter().enumerate() {
        match tok.kind {
            TokKind::Punct => match &text[tok.range.clone()] {
                "(" | "[" | "{" => depth += 1,
                ")" | "]" | "}" => depth = depth.saturating_sub(1),
                _ => {}
            },
            TokKind::Word if depth == 0 && is_word(text, tok, "FROM") => return Some(i),
            TokKind::Word | TokKind::Number | TokKind::Str | TokKind::Ident => {}
        }
    }
    None
}

/// A table name at the start of `toks`, then an index name if a `.` follows it.
fn target(text: &str, toks: &[Tok]) -> Option<Target> {
    let table = name(text, toks.first()?)?;
    let dotted = toks.get(1).is_some_and(|t| &text[t.range.clone()] == ".");
    let index = if dotted {
        toks.get(2).and_then(|t| name(text, t))
    } else {
        None
    };
    Some(Target { table, index })
}

/// The projected names, only when every comma-separated item is exactly one name token.
fn projection(text: &str, toks: &[Tok]) -> Option<Vec<String>> {
    if toks.is_empty() {
        return None;
    }
    toks.split(|t| &text[t.range.clone()] == ",")
        .map(|item| match item {
            [tok] => name(text, tok),
            _ => None,
        })
        .collect()
}

/// A bare word as written, or a quoted name with its quotes removed and `""` made `"`.
fn name(text: &str, tok: &Tok) -> Option<String> {
    let raw = &text[tok.range.clone()];
    match tok.kind {
        TokKind::Word => Some(raw.to_owned()),
        TokKind::Ident => Some(raw[1..raw.len() - 1].replace("\"\"", "\"")),
        TokKind::Number | TokKind::Str | TokKind::Punct => None,
    }
}

fn is_word(text: &str, tok: &Tok, word: &str) -> bool {
    tok.kind == TokKind::Word && text[tok.range.clone()].eq_ignore_ascii_case(word)
}

#[cfg(test)]
mod tests;
