//! A reply's rows as a stream. The reply is whole in memory, so its shaped rows are re-pushed
//! under the row and byte limits up front; a row cut under `Park` keeps the rest for `extend`.

use std::collections::VecDeque;

use super::shape::Shaped;
use crate::session::{ExtendError, Limits, OnTruncate, Outcome, StreamItem, TruncatedBy};
use crate::value::{BatchBuilder, CellRef, ColumnInfo, ResultBatch};

pub(crate) struct RedisStream {
    /// Batches cut to the limits, ready to deliver.
    ready: VecDeque<ResultBatch>,
    /// Shaped rows not delivered yet; the front batch starts at row `offset`.
    rest: VecDeque<ResultBatch>,
    offset: usize,
    builder: BatchBuilder,
    limits: Limits,
    delivered: u64,
    /// The shaping itself stopped at `max_rows`, so more elements existed than it kept.
    shaped_truncated: bool,
    /// A row cut under `Park` with a row left: `extend` may go on.
    parked: bool,
    outcome: Outcome,
}

impl RedisStream {
    /// The columns of `shaped`, and a stream of its rows under `limits`, ending with
    /// `rows_affected`.
    pub(crate) fn new(
        shaped: Shaped,
        limits: Limits,
        rows_affected: Option<u64>,
    ) -> (Vec<ColumnInfo>, Self) {
        let mut stream = RedisStream {
            ready: VecDeque::new(),
            rest: shaped.batches.into(),
            offset: 0,
            builder: BatchBuilder::new(shaped.columns.len()),
            limits,
            delivered: 0,
            shaped_truncated: shaped.truncated,
            parked: false,
            outcome: Outcome {
                rows: 0,
                truncated: false,
                rows_affected,
                truncated_by: None,
                extra_results: 0,
                rolled_back: false,
            },
        };
        stream.fill();
        (shaped.columns, stream)
    }

    /// Whether a limit cut the reply short.
    pub(crate) fn truncated(&self) -> bool {
        self.outcome.truncated
    }

    pub(crate) fn next_event(&mut self) -> StreamItem {
        match self.ready.pop_front() {
            Some(batch) => StreamItem::Batch(batch),
            None => StreamItem::Done(self.outcome.clone()),
        }
    }

    /// Raises a parked stream's row cap, or lifts it for `None`. Only a row cut under `Park`
    /// parks: a byte cut or a finished reply has nothing to go on with.
    pub(crate) fn extend(&mut self, more: Option<u64>) -> Result<(), ExtendError> {
        if !self.parked {
            return Err(ExtendError::NotParked);
        }
        self.limits.max_rows = more.and_then(|m| self.limits.max_rows.map(|r| r.saturating_add(m)));
        self.parked = false;
        self.fill();
        Ok(())
    }

    /// Moves rows from `rest` into ready batches until a limit stops one with a row left, then
    /// records the outcome. The cut is checked before every row, so a cut proves a row remains.
    fn fill(&mut self) {
        let mut cut = None;
        while let Some(source) = self.rest.front() {
            if self.offset == source.len() {
                self.rest.pop_front();
                self.offset = 0;
                continue;
            }
            cut = self.limits.cut(self.delivered, self.builder.total_bytes());
            if cut.is_some() {
                break;
            }
            let r = self.offset;
            let cells: Vec<CellRef<'_>> = (0..source.column_count())
                .map(|c| source.cell(r, c))
                .collect();
            let closed = self
                .builder
                .push_typed_row(&cells, |c| source.is_number(r, c));
            self.ready.extend(closed);
            if self.builder.is_full() {
                self.ready.push_back(self.builder.finish());
            }
            self.offset += 1;
            self.delivered += 1;
        }
        if !self.builder.is_empty() {
            self.ready.push_back(self.builder.finish());
        }
        self.parked = cut == Some(TruncatedBy::Rows) && self.limits.on_truncate == OnTruncate::Park;
        if !self.parked {
            // Nothing can go on from here, so the undelivered rows are let go.
            self.rest.clear();
        }
        let truncated_by = cut.or(self.shaped_truncated.then_some(TruncatedBy::Rows));
        self.outcome.rows = self.delivered;
        self.outcome.truncated = truncated_by.is_some();
        self.outcome.truncated_by = truncated_by;
    }
}
