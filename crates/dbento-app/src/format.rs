//! What Format SQL rewrites: the same range a run would send, in Dbento's house style.

use std::ops::Range;

use dbento_core::{Engine, FormatOptions, format_sql, splits_token};

use crate::run_target::run_range;

/// The edit Format SQL makes: the range `run_range` picks and its formatted text; None when
/// there is nothing to format, the text is already formatted, or a selection cuts a token of
/// the whole text (formatting half a string literal would recase its contents).
pub fn format_edit(
    engine: Engine,
    text: &str,
    selection: Range<usize>,
) -> Option<(Range<usize>, String)> {
    let range = run_range(engine, text, selection.clone())?;
    if range == selection && splits_token(engine, text, range.clone()) {
        return None;
    }
    let old = &text[range.clone()];
    let new = format_sql(engine, old, &FormatOptions::default());
    (new != old).then_some((range, new))
}
