//! The statement splitter: a script's tokens grouped into one piece per shell statement.

use std::ops::Range;

use super::lex::{Kind, LexError, Tok, lex};

/// One statement: its source `range` (with a closing `;`), its tokens (without it), and the
/// lex error that ended the script inside it, if any.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Piece {
    pub(crate) range: Range<usize>,
    pub(crate) toks: Range<usize>,
    pub(crate) error: Option<LexError>,
}

/// The script's tokens and its non-empty statement pieces, in order.
///
/// At bracket depth 0 a `;` ends a statement, and so does a line break unless a `.` sits on
/// either side of it, which carries a method chain onto the next line.
pub(crate) fn split(src: &str) -> (Vec<Tok>, Vec<Piece>) {
    let (toks, error) = lex(src);
    let mut pieces = Vec::new();
    // The open piece's first token index, and its first byte.
    let mut open: Option<(usize, usize)> = None;
    let mut depth = 0usize;
    let mut close = |open: &mut Option<(usize, usize)>, end_tok: usize, end_byte: usize| {
        if let Some((first, start)) = open.take() {
            pieces.push(Piece {
                range: start..end_byte,
                toks: first..end_tok,
                error: None,
            });
        }
    };
    let is_dot = |i: usize| toks[i].kind == Kind::Punct(b'.');
    for (i, tok) in toks.iter().enumerate() {
        if depth == 0 && tok.line_break_before && !is_dot(i) && !(i > 0 && is_dot(i - 1)) {
            close(&mut open, i, toks[i - 1].range.end);
        }
        match tok.kind {
            Kind::Punct(b';') if depth == 0 => {
                close(&mut open, i, tok.range.end);
                continue;
            }
            Kind::Punct(b'(' | b'[' | b'{') => depth += 1,
            Kind::Punct(b')' | b']' | b'}') => depth = depth.saturating_sub(1),
            Kind::Ident | Kind::Str | Kind::Num | Kind::Regex | Kind::Punct(_) => {}
        }
        open.get_or_insert((i, tok.range.start));
    }
    match error {
        // A lex error stands where its token would have, then owns the rest of the script.
        Some(error) => {
            let prev_end = toks.last().map_or(0, |t| t.range.end);
            let breaks = !toks.is_empty() && src[prev_end..error.at].contains('\n');
            let after_dot = toks.last().is_some_and(|t| t.kind == Kind::Punct(b'.'));
            if depth == 0 && breaks && !after_dot {
                close(&mut open, toks.len(), prev_end);
            }
            let (first, start) = open.unwrap_or((toks.len(), error.at));
            let end = src.trim_end().len();
            pieces.push(Piece {
                range: start..end,
                toks: first..toks.len(),
                error: Some(error),
            });
        }
        None => close(
            &mut open,
            toks.len(),
            toks.last().map_or(0, |t| t.range.end),
        ),
    }
    (toks, pieces)
}

/// The byte range of each statement in `src`, for the editor's run-at-cursor.
pub(crate) fn statement_ranges(src: &str) -> Vec<Range<usize>> {
    split(src).1.into_iter().map(|piece| piece.range).collect()
}

#[cfg(test)]
mod tests;
