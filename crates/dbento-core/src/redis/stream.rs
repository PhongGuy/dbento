//! A reply's rows as a stream: the reply is whole in memory, so its batches are served as shaped.

use std::collections::VecDeque;

use super::shape::Shaped;
use crate::session::{ExtendError, Outcome, StreamItem};
use crate::value::{ColumnInfo, ResultBatch};

pub(crate) struct RedisStream {
    batches: VecDeque<ResultBatch>,
    outcome: Outcome,
}

impl RedisStream {
    /// The columns of `shaped`, and a stream of its batches ending with `rows_affected`.
    pub(crate) fn new(shaped: Shaped, rows_affected: Option<u64>) -> (Vec<ColumnInfo>, Self) {
        let outcome = Outcome {
            rows: shaped.rows,
            truncated: false,
            rows_affected,
            truncated_by: None,
            extra_results: 0,
            rolled_back: false,
        };
        let stream = RedisStream {
            batches: shaped.batches.into(),
            outcome,
        };
        (shaped.columns, stream)
    }

    pub(crate) fn next_event(&mut self) -> StreamItem {
        match self.batches.pop_front() {
            Some(batch) => StreamItem::Batch(batch),
            None => StreamItem::Done(self.outcome.clone()),
        }
    }

    /// Nothing parks yet: every reply is delivered whole.
    pub(crate) fn extend(&mut self, _more: Option<u64>) -> Result<(), ExtendError> {
        Err(ExtendError::NotParked)
    }
}
