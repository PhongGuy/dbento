//! An S3 command's rows, delivered within the row and byte limits; a row cut parks the result
//! so it can resume where it stopped.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::aws::AwsCancel;
use crate::session::{
    ExecuteError, ExtendError, Limits, OnTruncate, Outcome, StreamItem, TruncatedBy,
};
use crate::value::{BatchBuilder, CellRef};

/// What a session and its streams share.
pub(crate) struct Shared {
    pub(crate) timeout: Option<Duration>,
    pub(crate) closed: AtomicBool,
    pub(crate) cancel: Arc<AwsCancel>,
}

impl Shared {
    pub(crate) fn new(timeout: Option<Duration>) -> Self {
        Shared {
            timeout,
            closed: AtomicBool::new(false),
            cancel: Arc::new(AwsCancel::new()),
        }
    }

    /// Runs one call under the session timeout and the cancel handle, which drops it in flight.
    pub(crate) async fn bounded<T>(
        &self,
        call: impl Future<Output = Result<T, ExecuteError>>,
    ) -> Result<T, ExecuteError> {
        let timed = async {
            match self.timeout {
                Some(timeout) => tokio::time::timeout(timeout, call)
                    .await
                    .unwrap_or(Err(ExecuteError::Timeout)),
                None => call.await,
            }
        };
        self.cancel.guard(timed).await?
    }
}

/// One cell of an S3 row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum S3Cell {
    Null,
    Text(String),
    Bytes(Vec<u8>),
}

impl S3Cell {
    fn as_cell(&self) -> CellRef<'_> {
        match self {
            S3Cell::Null => CellRef::Null,
            S3Cell::Text(text) => CellRef::Text(text),
            S3Cell::Bytes(bytes) => CellRef::Bytes(bytes),
        }
    }
}

pub(crate) type Row = Vec<S3Cell>;

pub(crate) struct S3Stream {
    builder: BatchBuilder,
    buffer: VecDeque<Row>,
    /// The token of the next listing page; None once the server sent the last one.
    next_token: Option<String>,
    limits: Limits,
    delivered: u64,
    /// A row cut parked the result: the row that proved it heads the buffer.
    parked: bool,
    done: Option<Outcome>,
    shared: Arc<Shared>,
}

impl S3Stream {
    /// A stream over rows already fetched, `columns` cells wide.
    pub(crate) fn buffered(
        columns: usize,
        rows: Vec<Row>,
        limits: Limits,
        shared: Arc<Shared>,
    ) -> Self {
        S3Stream {
            builder: BatchBuilder::new(columns),
            buffer: rows.into(),
            next_token: None,
            limits,
            delivered: 0,
            parked: false,
            done: None,
            shared,
        }
    }

    /// The next batch or the end. A failure drops the buffer and the token.
    pub(crate) fn next_event(&mut self) -> Result<StreamItem, ExecuteError> {
        let event = self.read();
        if event.is_err() {
            self.stop();
        }
        event
    }

    fn read(&mut self) -> Result<StreamItem, ExecuteError> {
        let done = loop {
            if let Some(done) = &self.done {
                break done.clone();
            }
            let Some(row) = self.buffer.pop_front() else {
                self.done = Some(self.outcome(None));
                continue;
            };
            // A row past a reached bound proves the result was cut short.
            if let Some(cut) = self.limits.cut(self.delivered, self.builder.total_bytes()) {
                if cut == TruncatedBy::Rows && self.limits.on_truncate == OnTruncate::Park {
                    self.buffer.push_front(row);
                    self.parked = true;
                } else {
                    self.stop();
                }
                if self.limits.on_truncate == OnTruncate::Close {
                    self.shared.closed.store(true, Ordering::Relaxed);
                }
                self.done = Some(self.outcome(Some(cut)));
                continue;
            }
            let cells: Vec<CellRef<'_>> = row.iter().map(S3Cell::as_cell).collect();
            let closed = self.builder.push_row(&cells);
            self.delivered += 1;
            if let Some(batch) = closed {
                return Ok(StreamItem::Batch(batch));
            }
            if self.builder.is_full() {
                return Ok(StreamItem::Batch(self.builder.finish()));
            }
        };
        if self.builder.is_empty() {
            Ok(StreamItem::Done(done))
        } else {
            Ok(StreamItem::Batch(self.builder.finish()))
        }
    }

    /// Raises a parked stream's row cap, or lifts it for `None`. Only a row cut parks.
    pub(crate) fn extend(&mut self, more: Option<u64>) -> Result<(), ExtendError> {
        if !self.parked {
            return Err(ExtendError::NotParked);
        }
        self.parked = false;
        self.limits.max_rows = more.and_then(|m| self.limits.max_rows.map(|r| r.saturating_add(m)));
        self.done = None;
        Ok(())
    }

    fn outcome(&self, cut: Option<TruncatedBy>) -> Outcome {
        Outcome {
            rows: self.delivered,
            truncated: cut.is_some(),
            rows_affected: None,
            truncated_by: cut,
            extra_results: 0,
            rolled_back: false,
            consumed_capacity: None,
        }
    }

    fn stop(&mut self) {
        self.next_token = None;
        self.buffer.clear();
        self.parked = false;
    }
}
