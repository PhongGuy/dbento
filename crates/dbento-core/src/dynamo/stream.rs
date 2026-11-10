//! A PartiQL statement's rows: pages fetched only as far as the row and byte limits need,
//! with the `NextToken` kept so a parked result can resume where it stopped.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use aws_sdk_dynamodb::types::AttributeValue;
use tokio::sync::watch;

use super::client::{DynamoClient, StatementRequest};
use super::columns::{self, ColumnModel, Item};
use super::errors::{execute_error, stale_token};
use super::statement::Kind;
use crate::capacity::CapacityUnits;
use crate::session::{
    ExecuteError, ExtendError, Limits, OnTruncate, Outcome, StreamItem, TruncatedBy,
};
use crate::value::{BatchBuilder, ColumnInfo, MAX_BATCH_BYTES};

/// Cancels by generation: `cancel` bumps it, a delivered cancel or `spend` spends it.
#[derive(Debug)]
pub(crate) struct DynamoCancel {
    generation: watch::Sender<u64>,
    spent: AtomicU64,
}

impl DynamoCancel {
    pub(crate) fn new() -> Self {
        DynamoCancel {
            generation: watch::Sender::new(0),
            spent: AtomicU64::new(0),
        }
    }

    pub(crate) fn cancel(&self) {
        // `send_modify` counts the cancel even with no statement listening.
        self.generation.send_modify(|g| *g += 1);
    }

    pub(crate) fn spend(&self) {
        let generation = *self.generation.borrow();
        self.spent.store(generation, Ordering::Relaxed);
    }

    /// `Err(Cancelled)` at once for an unspent cancel, or when one arrives while `f` runs, which
    /// drops `f`.
    pub(crate) async fn guard<F: Future>(&self, f: F) -> Result<F::Output, ExecuteError> {
        let mut generation = self.generation.subscribe();
        // Discards `wait_for`'s reply here: it holds the channel's lock, which `spend` needs.
        let cancelled = async {
            let unspent = |g: &u64| *g != self.spent.load(Ordering::Relaxed);
            let _ = generation.wait_for(unspent).await;
        };
        // Biased so an unspent cancel wins before `f` is first polled, and nothing is sent.
        tokio::select! {
            biased;
            () = cancelled => {
                self.spend();
                Err(ExecuteError::Cancelled)
            }
            out = f => Ok(out),
        }
    }
}

/// What a session and its streams share.
pub(crate) struct Shared {
    pub(crate) timeout: Option<Duration>,
    pub(crate) closed: AtomicBool,
    pub(crate) cancel: Arc<DynamoCancel>,
    /// The item bytes one fetch may buffer before it pauses.
    batch_cap: usize,
}

impl Shared {
    pub(crate) fn new(timeout: Option<Duration>) -> Self {
        Shared {
            timeout,
            closed: AtomicBool::new(false),
            cancel: Arc::new(DynamoCancel::new()),
            batch_cap: MAX_BATCH_BYTES,
        }
    }

    /// Test seam: a smaller fetch byte cap than MAX_BATCH_BYTES.
    #[cfg(test)]
    pub(crate) fn with_batch_cap(self, bytes: usize) -> Self {
        Shared {
            batch_cap: bytes,
            ..self
        }
    }
}

/// One statement as it is sent for every page.
pub(crate) struct Request {
    pub(crate) kind: Kind,
    pub(crate) text: String,
    pub(crate) consistent_read: bool,
}

/// What fixes the columns: the statement's projection, if it names one, and the table's keys.
pub(crate) struct Shape<'a> {
    pub(crate) projection: Option<&'a [String]>,
    pub(crate) keys: &'a [String],
}

pub(crate) struct DynamoStream {
    client: Arc<DynamoClient>,
    request: Request,
    model: ColumnModel,
    builder: BatchBuilder,
    buffer: VecDeque<Item>,
    /// The token of the next page; None once the server sent the last one or a cut dropped it.
    next_token: Option<String>,
    limits: Limits,
    delivered: u64,
    capacity: Option<CapacityUnits>,
    /// A row cut parked the result: the row that proved it heads the buffer.
    parked: bool,
    done: Option<Outcome>,
    shared: Arc<Shared>,
}

impl DynamoStream {
    /// Sends the statement and fetches the pages that fix the columns.
    pub(crate) async fn open(
        client: Arc<DynamoClient>,
        request: Request,
        shape: Shape<'_>,
        limits: Limits,
        shared: Arc<Shared>,
    ) -> Result<(Vec<ColumnInfo>, Self), ExecuteError> {
        let kind = request.kind;
        let mut stream = DynamoStream {
            client,
            request,
            model: ColumnModel::of(&[], Some(&[]), &[], true).0,
            builder: columns::builder(0),
            buffer: VecDeque::new(),
            next_token: None,
            limits,
            delivered: 0,
            capacity: None,
            parked: false,
            done: None,
            shared,
        };
        stream.fetch().await?;
        let ended = stream.next_token.is_none();
        // A write without RETURNING answers with nothing at all: no columns, no rows.
        if kind != Kind::Select && ended && stream.buffer.is_empty() {
            stream.done = Some(stream.outcome(None));
            return Ok((Vec::new(), stream));
        }
        let first = stream.buffer.make_contiguous();
        let (model, columns) = ColumnModel::of(first, shape.projection, shape.keys, ended);
        stream.model = model;
        stream.builder = columns::builder(columns.len());
        Ok((columns, stream))
    }

    /// One fetch under the statement timeout, which spans all its pages, and the cancel handle,
    /// which drops the request in flight.
    async fn fetch(&mut self) -> Result<(), ExecuteError> {
        let shared = Arc::clone(&self.shared);
        let pages = self.fetch_pages();
        let bounded = async {
            match shared.timeout {
                Some(timeout) => tokio::time::timeout(timeout, pages)
                    .await
                    .unwrap_or(Err(ExecuteError::Timeout)),
                None => pages.await,
            }
        };
        shared.cancel.guard(bounded).await?
    }

    /// Requests pages, starting at the held token, until the buffer holds the row after the
    /// limit, reaches the byte bound, or the token runs out. Pages without items are followed.
    async fn fetch_pages(&mut self) -> Result<(), ExecuteError> {
        let byte_bound = self.limits.max_bytes.map_or(self.shared.batch_cap, |b| {
            usize::try_from(b).map_or(self.shared.batch_cap, |b| b.min(self.shared.batch_cap))
        });
        let mut bytes = 0;
        loop {
            let request = StatementRequest {
                kind: self.request.kind,
                text: &self.request.text,
                next_token: self.next_token.as_deref(),
                consistent_read: self.request.consistent_read,
            };
            // A continuation's ValidationException most likely means the token went stale.
            let continued = request.next_token.is_some();
            let page = self.client.execute_statement(request).await.map_err(|e| {
                let error = execute_error(e);
                if continued { stale_token(error) } else { error }
            })?;
            if let Some(units) = page.consumed_capacity.and_then(|c| c.capacity_units) {
                let units = CapacityUnits::from_units(units);
                self.capacity = Some(self.capacity.map_or(units, |sum| sum.saturating_add(units)));
            }
            for item in page.items.unwrap_or_default() {
                bytes += item_size(&item);
                self.buffer.push_back(item);
            }
            self.next_token = page.next_token;
            let held = self.delivered + self.buffer.len() as u64;
            let rows_proved = self.limits.max_rows.is_some_and(|r| held > r);
            // The byte stop needs a held item, or a zero byte limit would end on an empty page.
            let bytes_reached = bytes >= byte_bound && !self.buffer.is_empty();
            if self.next_token.is_none() || rows_proved || bytes_reached {
                return Ok(());
            }
        }
    }

    /// The next batch or the end. A failure drops the buffer and the token.
    pub(crate) async fn next_event(&mut self) -> Result<StreamItem, ExecuteError> {
        let event = self.read().await;
        if event.is_err() {
            self.stop();
        }
        event
    }

    async fn read(&mut self) -> Result<StreamItem, ExecuteError> {
        let done = loop {
            if let Some(done) = &self.done {
                break done.clone();
            }
            let Some(item) = self.next_item().await? else {
                self.done = Some(self.outcome(None));
                continue;
            };
            // An item past a reached bound proves the result was cut short.
            if let Some(cut) = self.limits.cut(self.delivered, self.builder.total_bytes()) {
                if cut == TruncatedBy::Rows && self.limits.on_truncate == OnTruncate::Park {
                    self.buffer.push_front(item);
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
            let closed = self.model.push(&mut self.builder, &item);
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

    /// The buffered item, else the first item of a further fetch while a token remains.
    async fn next_item(&mut self) -> Result<Option<Item>, ExecuteError> {
        if self.buffer.is_empty() && self.next_token.is_some() {
            self.fetch().await?;
        }
        Ok(self.buffer.pop_front())
    }

    fn outcome(&self, cut: Option<TruncatedBy>) -> Outcome {
        Outcome {
            rows: self.delivered,
            truncated: cut.is_some(),
            rows_affected: None,
            truncated_by: cut,
            extra_results: 0,
            rolled_back: false,
            consumed_capacity: self.capacity,
        }
    }

    fn stop(&mut self) {
        self.next_token = None;
        self.buffer.clear();
        self.parked = false;
    }
}

/// An estimate of an item's size (names plus values), used only to decide when to stop fetching.
fn item_size(item: &Item) -> usize {
    item.iter()
        .map(|(name, value)| name.len() + value_size(value))
        .sum()
}

fn value_size(value: &AttributeValue) -> usize {
    match value {
        AttributeValue::S(text) | AttributeValue::N(text) => text.len(),
        AttributeValue::B(blob) => blob.as_ref().len(),
        AttributeValue::Ss(texts) | AttributeValue::Ns(texts) => {
            texts.iter().map(String::len).sum()
        }
        AttributeValue::Bs(blobs) => blobs.iter().map(|b| b.as_ref().len()).sum(),
        AttributeValue::M(map) => item_size(map),
        AttributeValue::L(values) => values.iter().map(value_size).sum(),
        AttributeValue::Bool(_) | AttributeValue::Null(_) | AttributeValue::Unknown { .. } => 1,
        // AttributeValue is non-exhaustive; a type added later counts as a scalar.
        _ => 1,
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod deadline_tests;
