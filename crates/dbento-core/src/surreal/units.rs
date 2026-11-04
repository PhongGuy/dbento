//! Splits a SurrealQL script into units, the pieces `execute` sends one call at a time.
//! A transaction span or a `LET` keeps later statements in its own call, and a literal
//! top-level `USE` becomes no text at all. The driver spec's "Scripts and units" has the rules.

use std::ops::Range;

use super::lex::{Kind, LexError, Tok, alternate, lex, on_path};

/// A top-level `USE` wholly made of literal names; `None` keeps that part of the session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct UseTarget {
    pub(crate) namespace: Option<String>,
    pub(crate) database: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum UnitKind {
    /// Text sent as written.
    Text,
    /// No text: a pre-check, then RPC `use`.
    Use(UseTarget),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Unit {
    /// From the first token to the closing `;` or last token; to the end of the text after a
    /// lex error.
    pub(crate) range: Range<usize>,
    /// Non-empty top-level statements in the unit.
    pub(crate) statements: usize,
    pub(crate) kind: UnitKind,
    /// The unit holds a `USE` that `execute` must refuse: inside a multi-statement unit, nested,
    /// or with a target the literal grammar does not wholly consume.
    pub(crate) refused_use: bool,
    /// An unterminated token ended the unit.
    pub(crate) lex_error: bool,
}

/// One top-level statement: its tokens, closing `;` included, and its bytes.
struct Statement {
    toks: Range<usize>,
    range: Range<usize>,
    lex_error: bool,
}

impl Statement {
    fn headed(&self, toks: &[Tok], word: &str) -> bool {
        toks[self.toks.clone()]
            .first()
            .is_some_and(|tok| tok.is_word(word))
    }
}

/// The script's units in order; a script of only comments and `;` has none.
pub(crate) fn units(src: &str) -> Vec<Unit> {
    let (toks, error) = lex(src);
    let second = alternate(src);
    let doubt = second
        .as_deref()
        .and_then(|second| first_doubt(&toks, second));
    let statements = statements(src, &toks, error);
    let mut out = Vec::new();
    let mut first = 0;
    while first < statements.len() {
        let mut last = span_end(&statements, &toks, first);
        // Past the doubt no cut can be trusted, so the unit holding it takes the rest.
        if let Some(doubt) = doubt
            && statements[first].range.start <= doubt
            && statements
                .get(last + 1)
                .is_none_or(|next| next.range.start > doubt)
        {
            last = statements.len() - 1;
        }
        out.push(unit(
            src,
            &toks,
            second.as_deref(),
            &statements[first],
            &statements[last],
            last - first + 1,
        ));
        first = last + 1;
    }
    out
}

/// Each unit's byte range, as `statement_ranges` reports them.
pub(crate) fn statement_ranges(src: &str) -> Vec<Range<usize>> {
    units(src).into_iter().map(|unit| unit.range).collect()
}

/// How many top-level statements the script holds, by the lexer reading that finds more; the
/// reading the server took agrees with `statements` in its `begin` frame.
pub(crate) fn statement_count(src: &str) -> usize {
    let primary = units(src).iter().map(|unit| unit.statements).sum();
    let second = alternate(src).map_or(0, |second| statements(src, &second, None).len());
    usize::max(primary, second)
}

// The first byte where the two lexer readings cut the text differently.
fn first_doubt(toks: &[Tok], second: &[Tok]) -> Option<usize> {
    let at = |toks: &[Tok]| -> Vec<usize> {
        cuts(toks)
            .into_iter()
            .map(|cut| toks[cut].range.start)
            .collect()
    };
    let (ours, theirs) = (at(toks), at(second));
    let same = ours.iter().zip(&theirs).take_while(|(a, b)| a == b).count();
    ours.get(same)
        .into_iter()
        .chain(theirs.get(same))
        .min()
        .copied()
}

// The token index of every `;` outside brackets. A stray closer must not hide a later cut.
fn cuts(toks: &[Tok]) -> Vec<usize> {
    let mut depth = 0usize;
    let mut out = Vec::new();
    for (i, tok) in toks.iter().enumerate() {
        match tok.kind {
            Kind::Punct('(' | '[' | '{') => depth += 1,
            Kind::Punct(')' | ']' | '}') => depth = depth.saturating_sub(1),
            Kind::Punct(';') if depth == 0 => out.push(i),
            _ => {}
        }
    }
    out
}

// Cuts the tokens into non-empty statements. A lex error ends the text inside an open
// statement, or else as a statement of its own, with no tokens, that a span can still hold.
fn statements(src: &str, toks: &[Tok], error: Option<LexError>) -> Vec<Statement> {
    let piece = |at: Range<usize>| Statement {
        range: toks[at.start].range.start..toks[at.end - 1].range.end,
        toks: at,
        lex_error: false,
    };
    let mut out = Vec::new();
    let mut start = 0;
    for cut in cuts(toks) {
        if cut > start {
            out.push(piece(start..cut + 1));
        }
        start = cut + 1;
    }
    let open = start < toks.len();
    if open {
        out.push(piece(start..toks.len()));
    }
    if let Some(error) = error {
        if !open {
            out.push(Statement {
                toks: toks.len()..toks.len(),
                range: error.at..src.len(),
                lex_error: true,
            });
        } else if let Some(last) = out.last_mut() {
            last.range.end = src.len();
            last.lex_error = true;
        }
    }
    out
}

// The index of the last statement in the unit starting at `first`. A `LET` must share its
// call with whatever reads it, so one anywhere in a span holds everything after it too.
fn span_end(statements: &[Statement], toks: &[Tok], first: usize) -> usize {
    let to_the_end = statements.len() - 1;
    if statements[first].headed(toks, "let") {
        return to_the_end;
    }
    if !statements[first].headed(toks, "begin") {
        return first;
    }
    for (i, statement) in statements.iter().enumerate().skip(first + 1) {
        if statement.headed(toks, "let") {
            return to_the_end;
        }
        if statement.headed(toks, "commit") || statement.headed(toks, "cancel") {
            return i;
        }
    }
    to_the_end
}

fn unit(
    src: &str,
    toks: &[Tok],
    second: Option<&[Tok]>,
    first: &Statement,
    last: &Statement,
    statements: usize,
) -> Unit {
    let range = first.range.start..last.range.end;
    let toks = &toks[first.toks.start..last.toks.end];
    let target = (statements == 1 && !last.lex_error)
        .then(|| literal_use(src, toks))
        .flatten();
    // Either reading refuses: a wrong guess at a `/` must not hide a `USE` the server may run.
    let refused_use = target.is_none()
        && (holds_refused_use(toks)
            || second.is_some_and(|second| holds_refused_use(within(second, &range))));
    Unit {
        range,
        statements,
        refused_use,
        kind: target.map_or(UnitKind::Text, UnitKind::Use),
        lex_error: last.lex_error,
    }
}

// The tokens of a reading that start inside `range`.
fn within<'a>(toks: &'a [Tok], range: &Range<usize>) -> &'a [Tok] {
    let from = toks.partition_point(|tok| tok.range.start < range.start);
    let to = toks.partition_point(|tok| tok.range.start < range.end);
    &toks[from..to]
}

// `USE [NS name] [DB name]`, with at least one part and no token left over: a parser that
// stopped early would take `d` from `USE DB d-2`, which the server reads as `d-2`.
fn literal_use(src: &str, toks: &[Tok]) -> Option<UseTarget> {
    let toks = match toks.split_last() {
        Some((last, rest)) if last.is_punct(';') => rest,
        _ => toks,
    };
    let (head, mut rest) = toks.split_first()?;
    if !head.is_word("use") {
        return None;
    }
    let namespace = named_part(src, &mut rest, ["ns", "namespace"])?;
    let database = named_part(src, &mut rest, ["db", "database"])?;
    let any = namespace.is_some() || database.is_some();
    (any && rest.is_empty()).then_some(UseTarget {
        namespace,
        database,
    })
}

// Takes `keyword name` from the front of `rest`. The outer `None` is a keyword with no
// literal name after it; the inner one is a part that is absent.
fn named_part(src: &str, rest: &mut &[Tok], keywords: [&str; 2]) -> Option<Option<String>> {
    let Some((keyword, after)) = rest.split_first() else {
        return Some(None);
    };
    if !keyword.is_any_word(&keywords) {
        return Some(None);
    }
    let (name, after) = after.split_first()?;
    *rest = after;
    literal_name(src, name).map(Some)
}

// A quoted name counts only if every escape in it is its own mark or a backslash, and `⟨⟩`
// has neither. The server decodes more, so anything else could name another database.
fn literal_name(src: &str, tok: &Tok) -> Option<String> {
    let raw = &src[tok.range.clone()];
    match tok.kind {
        Kind::Ident { .. } if !raw.starts_with('`') => tok
            .name()
            .filter(|name| !name.contains('\\'))
            .map(str::to_owned),
        Kind::Ident { .. } | Kind::Str { prefix: None } => unquoted(raw),
        _ => None,
    }
}

fn unquoted(quoted: &str) -> Option<String> {
    let mut chars = quoted.chars();
    let mark = chars.next()?;
    let body = chars.as_str().strip_suffix(mark)?;
    let mut out = String::with_capacity(body.len());
    let mut chars = body.chars();
    while let Some(c) = chars.next() {
        out.push(match c {
            '\\' => chars.next().filter(|&next| next == mark || next == '\\')?,
            _ => c,
        });
    }
    Some(out)
}

// An unquoted `USE` that starts a statement, or that a target keyword follows at any depth.
// After `.` or `::` the word is a field or path segment, and before `:` a key or record table.
fn holds_refused_use(toks: &[Tok]) -> bool {
    toks.iter().enumerate().any(|(i, tok)| {
        if !tok.is_word("use") {
            return false;
        }
        let prev = i.checked_sub(1).map(|p| &toks[p]);
        let next = toks.get(i + 1);
        let named = on_path(prev) || next.is_some_and(|n| n.is_punct(':'));
        let at_start = prev.is_none_or(|p| p.is_punct(';') || p.is_punct('{'));
        let targeted = next.is_some_and(|n| n.is_any_word(&["ns", "namespace", "db", "database"]));
        !named && (at_start || targeted)
    })
}

#[cfg(test)]
mod tests;
