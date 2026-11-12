//! The editor's line-comment toggle: comment out the lines a selection touches, or take the
//! marker back off when every one of them already has it.

use std::ops::Range;

/// One toggle's edit: replace `range` of the old text with `text`, then select `selection`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct CommentEdit {
    /// The range of the old text to replace: the touched lines, without the last line break.
    pub range: Range<usize>,
    /// The text that replaces it.
    pub text: String,
    /// The selection afterwards, in the new text, covering the same lines.
    pub selection: Range<usize>,
}

/// Toggles `marker` on the lines `selection` touches; a selection that ends at a line's start
/// leaves that line out. When every non-blank line starts with the marker after its
/// indentation, the marker and one space after it are removed; otherwise `marker` and a space
/// go in at the least indentation of the non-blank lines. Blank lines are never changed.
/// None when the lines are all blank.
pub fn toggle_line_comments(
    text: &str,
    selection: Range<usize>,
    marker: &str,
) -> Option<CommentEdit> {
    let from = selection.start.min(text.len());
    let to = selection.end.clamp(from, text.len());
    let first = text[..from].rfind('\n').map_or(0, |n| n + 1);
    let last = if to > from && text[..to].ends_with('\n') {
        to - 1
    } else {
        to
    };
    let end = text[last..].find('\n').map_or(text.len(), |n| last + n);

    // Each non-blank line as its absolute start and its indentation width in bytes.
    let mut lines = Vec::new();
    let mut start = first;
    for line in text[first..end].split('\n') {
        if !line.trim().is_empty() {
            let indent = line.len() - line.trim_start_matches([' ', '\t']).len();
            lines.push((start, indent));
        }
        start += line.len() + 1;
    }
    let column = lines.iter().map(|&(_, indent)| indent).min()?;

    // The edits in text order, as (old range, inserted text); one per non-blank line.
    let commented = lines
        .iter()
        .all(|&(at, indent)| text[at + indent..].starts_with(marker));
    let insert = format!("{marker} ");
    let edits: Vec<(Range<usize>, &str)> = lines
        .iter()
        .map(|&(at, indent)| {
            if commented {
                let cut = at + indent;
                let space = usize::from(text[cut + marker.len()..].starts_with(' '));
                (cut..cut + marker.len() + space, "")
            } else {
                (at + column..at + column, insert.as_str())
            }
        })
        .collect();

    let mut new = String::with_capacity(end - first + edits.len() * insert.len());
    let mut copied = first;
    for (range, with) in &edits {
        new.push_str(&text[copied..range.start]);
        new.push_str(with);
        copied = range.end;
    }
    new.push_str(&text[copied..end]);

    // A caret moves with its text; a selection's start stays before a marker inserted right at
    // it, so a selection from a line's start takes the new marker in.
    let caret = from == to;
    let selection = shift(from, &edits, caret)..shift(to, &edits, true);
    Some(CommentEdit {
        range: first..end,
        text: new,
        selection,
    })
}

// Where old offset `at` lands once `edits` apply; `past_insert` moves it past text inserted
// exactly at it.
fn shift(at: usize, edits: &[(Range<usize>, &str)], past_insert: bool) -> usize {
    let mut out = at;
    for (range, with) in edits {
        if range.is_empty() {
            if at > range.start || (at == range.start && past_insert) {
                out += with.len();
            }
        } else if at >= range.end {
            out -= range.len();
        } else if at > range.start {
            out -= at - range.start;
        }
    }
    out
}
