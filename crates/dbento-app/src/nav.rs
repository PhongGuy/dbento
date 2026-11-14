//! A tab's Back and Forward history: the reference hops that loaded it, with a cursor on the
//! one showing.

use dbento_core::RelationRef;

/// One hop: the relation a tab browsed and the WHERE text it browsed it with.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct Hop {
    /// The relation browsed.
    pub relation: RelationRef,
    /// The WHERE text of the browse.
    pub filter: String,
    /// The row to select once the browse lands; None selects a lone row.
    pub row: Option<usize>,
}

impl Hop {
    /// The hop to `relation` filtered by `filter`.
    pub fn new(relation: RelationRef, filter: String) -> Self {
        Hop {
            relation,
            filter,
            row: None,
        }
    }

    /// The same hop, landing on table row `row`.
    pub fn with_row(self, row: Option<usize>) -> Self {
        Hop { row, ..self }
    }
}

/// The hops of one tab, oldest first, and which of them is current.
#[derive(Debug, Clone, Default)]
#[non_exhaustive]
pub struct NavHistory {
    hops: Vec<Hop>,
    // Index of the current hop; meaningless while `hops` is empty.
    cursor: usize,
}

impl NavHistory {
    /// Drops every forward hop and makes `hop` current.
    pub fn push(&mut self, hop: Hop) {
        if !self.hops.is_empty() {
            self.hops.truncate(self.cursor + 1);
        }
        self.hops.push(hop);
        self.cursor = self.hops.len() - 1;
    }

    /// The hop showing, None before the first push.
    pub fn current(&self) -> Option<&Hop> {
        self.hops.get(self.cursor)
    }

    /// Whether a hop lies behind the current one.
    pub fn can_back(&self) -> bool {
        self.cursor > 0
    }

    /// Whether a hop lies ahead of the current one.
    pub fn can_forward(&self) -> bool {
        self.cursor + 1 < self.hops.len()
    }

    /// Steps and returns the new current hop; None (no step) at either end.
    pub fn back(&mut self) -> Option<Hop> {
        if !self.can_back() {
            return None;
        }
        self.cursor -= 1;
        self.current().cloned()
    }

    /// Steps forward and returns the new current hop; None (no step) at the newest hop.
    pub fn forward(&mut self) -> Option<Hop> {
        if !self.can_forward() {
            return None;
        }
        self.cursor += 1;
        self.current().cloned()
    }

    /// Forgets every hop.
    pub fn clear(&mut self) {
        self.hops.clear();
        self.cursor = 0;
    }
}
