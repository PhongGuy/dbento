//! The statement around the editor's caret, kept across keystrokes: a keystroke re-splits
//! from the last statement end the edit left intact, and only as far as the caret's statement
//! needs, so it never copies or lexes the whole buffer.

use std::ops::Range;

use dbento_core::{Engine, statement_ranges};
use gpui_kit::component::input::Rope;

use crate::completion::statement_base;

/// The first window split past the last intact statement end; it doubles until it holds the
/// caret's statement.
const WINDOW: usize = 4096;

/// Statement boundaries of one editor buffer, as [`statement_ranges`] draws them.
pub struct StatementIndex {
    engine: Engine,
    // The buffer the ends were found in; a clone of a rope shares its tree.
    text: Rope,
    // Ends of statements that no later text can move, ascending.
    ends: Vec<usize>,
    split: usize,
}

impl StatementIndex {
    /// An empty index for `engine`'s SQL.
    pub fn new(engine: Engine) -> Self {
        StatementIndex {
            engine,
            text: Rope::new(),
            ends: Vec::new(),
            split: 0,
        }
    }

    /// The span [`statement_base`] finds at `offset` of `text` from a full split. On
    /// Postgres a lexical error spreads only back to the last intact statement end, where a
    /// full split would make the whole text one statement.
    pub fn around(&mut self, text: &Rope, offset: usize) -> Range<usize> {
        self.keep_intact(text, offset);
        self.text = text.clone();
        self.split = 0;
        let len = text.len();
        let mut from = self.ends.last().copied().unwrap_or(0);
        let mut window = WINDOW;
        loop {
            let end = text.ceil_char_boundary((from + window).min(len));
            let piece = text.slice(from..end).to_string();
            self.split += piece.len();
            let ranges = statement_ranges(self.engine, &piece).into_iter();
            let ranges = ranges.map(|r| r.start + from..r.end + from);
            // A range reaching the window's edge may run on past it, or be a lexical error
            // the cut made; only ranges ending inside the window are settled.
            let settled: Vec<Range<usize>> = ranges.filter(|r| end == len || r.end < end).collect();
            let holds = |r: &Range<usize>| r.start >= offset || offset <= r.end;
            if end == len || settled.iter().any(holds) {
                // A statement running to the end of the text ends there only until more is typed.
                self.ends
                    .extend(settled.iter().map(|r| r.end).filter(|&e| e < len));
                let base = statement_base(&settled, offset, len);
                return base.start.max(from)..base.end;
            }
            // Statements done before the caret are kept, and the next window starts after
            // them; with none done, the caret's statement is longer than the window.
            let done = settled.iter().map(|r| r.end).filter(|&e| e < offset);
            let before = self.ends.len();
            self.ends.extend(done);
            match self.ends.get(before..).and_then(<[usize]>::last) {
                Some(&e) => from = e,
                None => window *= 2,
            }
        }
    }

    /// How many bytes the last [`Self::around`] split.
    pub fn last_split_bytes(&self) -> usize {
        self.split
    }

    // Drops every end the edit may have moved: an end stays only while the text before it
    // is unchanged, and only those up to the caret are wanted.
    fn keep_intact(&mut self, text: &Rope, offset: usize) {
        // A caret right after a `;` sits in the statement that `;` ends, so that end goes too.
        self.ends.retain(|&e| e < offset && e <= text.len());
        let same = |e: usize| self.text.slice(..e) == text.slice(..e);
        // Typing at the caret leaves the last end before it intact, the common case.
        if self.ends.last().is_some_and(|&e| same(e)) {
            return;
        }
        let keep = self.ends.partition_point(|&e| same(e));
        self.ends.truncate(keep);
    }
}
