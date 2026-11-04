//! Which part of the editor text a run sends: the selection, or the statement the caret
//! belongs to. Always an exact slice of the text, never rewritten.

use std::ops::Range;

use dbento_core::{Engine, statement_ranges};

/// The range of the statement the caret belongs to; None when the text has no statement.
/// A caret in a statement's body or right after its `;` picks it, the first of two touching
/// statements winning. In the gap before a body, the previous statement wins when no newline
/// lies between its end and the caret, else the next; after the last body, the last.
pub fn statement_at(engine: Engine, text: &str, caret: usize) -> Option<Range<usize>> {
    let ranges: Vec<_> = statement_ranges(engine, text)
        .into_iter()
        .filter(|r| body_start(text, r) < r.end)
        .collect();
    let mut prev: Option<&Range<usize>> = None;
    for r in &ranges {
        let body = body_start(text, r);
        if caret < body {
            return Some(match prev {
                Some(p) if !text[p.end..caret.max(p.end)].contains('\n') => p.clone(),
                _ => r.clone(),
            });
        }
        if caret <= r.end {
            return Some(r.clone());
        }
        prev = Some(r);
    }
    prev.cloned()
}

/// What Ctrl+Enter runs: a selection with non-blank text as is, else the statement at the
/// selection's start.
pub fn run_range(engine: Engine, text: &str, selection: Range<usize>) -> Option<Range<usize>> {
    match text.get(selection.clone()) {
        Some(s) if !s.trim().is_empty() => Some(selection),
        _ => statement_at(engine, text, selection.start),
    }
}

/// Where the statement in `r` starts once whitespace, comments and empty `;` are skipped.
/// An unterminated block comment runs to the end, as SQLite reads it.
fn body_start(text: &str, r: &Range<usize>) -> usize {
    let mut at = r.start;
    while at < r.end {
        let rest = &text[at..r.end];
        let skip = if rest.starts_with("--") {
            rest.find('\n').map_or(rest.len(), |n| n + 1)
        } else if let Some(inner) = rest.strip_prefix("/*") {
            inner.find("*/").map_or(rest.len(), |n| n + 4)
        } else {
            match rest.chars().next() {
                Some(c) if c.is_whitespace() || c == ';' => c.len_utf8(),
                _ => break,
            }
        };
        at += skip;
    }
    at
}
