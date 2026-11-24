//! An S3 command's rows, delivered within the row and byte limits; a row cut parks the result
//! so it can resume where it stopped.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::client::{ListCall, Listed, S3Client};
use super::errors::{STALE_TOKEN_HINT, classify, execute_error};
use crate::aws::AwsCancel;
use crate::session::{
    ExecuteError, ExtendError, Limits, OnTruncate, Outcome, StreamItem, TruncatedBy,
};
use crate::value::{BatchBuilder, CellRef, ColumnInfo};

/// The most keys one listing page asks for.
pub(crate) const PAGE_KEYS: u32 = 1000;

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

/// The columns of an `ls` over a bucket or prefix.
pub(crate) fn list_columns() -> Vec<ColumnInfo> {
    vec![
        ColumnInfo::new("kind", "text", None),
        ColumnInfo::new("key", "text", None),
        ColumnInfo::new("size", "integer", None),
        ColumnInfo::new("last_modified", "timestamp", None),
        ColumnInfo::new("etag", "text", None),
        ColumnInfo::new("storage_class", "text", None),
    ]
}

/// One page's prefixes and objects as rows in key byte order; on a tie the prefix comes first,
/// so an `a/` object still lists beside the `a/` prefix.
fn page_rows(page: Listed) -> Vec<Row> {
    let prefixes = page.prefixes.into_iter().map(|key| {
        let row = vec![
            S3Cell::Text("prefix".to_owned()),
            S3Cell::Text(key.clone()),
            S3Cell::Null,
            S3Cell::Null,
            S3Cell::Null,
            S3Cell::Null,
        ];
        (key, false, row)
    });
    let objects = page.objects.into_iter().map(|object| {
        let text = |value: Option<String>| value.map_or(S3Cell::Null, S3Cell::Text);
        let row = vec![
            S3Cell::Text("object".to_owned()),
            S3Cell::Text(object.key.clone()),
            S3Cell::Text(object.size.to_string()),
            text(object.last_modified),
            text(object.etag),
            text(object.storage_class),
        ];
        (object.key, true, row)
    });
    let mut keyed: Vec<(String, bool, Row)> = prefixes.chain(objects).collect();
    keyed.sort_by(|a, b| (&a.0, a.1).cmp(&(&b.0, b.1)));
    keyed.into_iter().map(|(_, _, row)| row).collect()
}

/// Where a listing's further pages come from; the token is the stream's.
struct Lister {
    client: Arc<S3Client>,
    call: ListCall,
}

pub(crate) struct S3Stream {
    builder: BatchBuilder,
    buffer: VecDeque<Row>,
    /// Set for a listing, which fetches page by page; None for rows fetched up front.
    lister: Option<Lister>,
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
            lister: None,
            next_token: None,
            limits,
            delivered: 0,
            parked: false,
            done: None,
            shared,
        }
    }

    /// A listing of `call`'s bucket and prefix, its first page already fetched.
    pub(crate) async fn listing(
        client: Arc<S3Client>,
        call: ListCall,
        limits: Limits,
        shared: Arc<Shared>,
    ) -> Result<Self, ExecuteError> {
        let mut stream = S3Stream::buffered(list_columns().len(), Vec::new(), limits, shared);
        stream.lister = Some(Lister { client, call });
        stream.fetch().await?;
        Ok(stream)
    }

    /// Fetches one page from the held token under the session timeout and cancel handle. A
    /// refused token most likely means the listing went stale, so the error says to list again.
    async fn fetch(&mut self) -> Result<(), ExecuteError> {
        let Some(lister) = self.lister.as_mut() else {
            return Ok(());
        };
        let continued = self.next_token.is_some();
        lister.call.token = self.next_token.take();
        let call = async {
            lister.client.list_objects(&lister.call).await.map_err(|e| {
                let mut error = execute_error(classify(e));
                if let ExecuteError::Database { code, hint, .. } = &mut error
                    && continued
                    && code == "InvalidArgument"
                {
                    *hint = Some(STALE_TOKEN_HINT.to_owned());
                }
                error
            })
        };
        let page = self.shared.bounded(call).await?;
        self.next_token = page.next_token.clone();
        self.buffer.extend(page_rows(page));
        Ok(())
    }

    /// The buffered row, else the first row of further pages while a token remains; a page
    /// with no rows is followed.
    async fn next_row(&mut self) -> Result<Option<Row>, ExecuteError> {
        while self.buffer.is_empty() && self.next_token.is_some() {
            self.fetch().await?;
        }
        Ok(self.buffer.pop_front())
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
            let Some(row) = self.next_row().await? else {
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

#[cfg(test)]
pub(crate) mod tests_support {
    use super::S3Stream;
    use crate::session::{Outcome, StreamItem};
    use crate::value::CellRef;

    /// The `key` column of every row until the stream ends, and how it ended.
    pub(crate) async fn drain(stream: &mut S3Stream) -> (Vec<String>, Outcome) {
        let mut keys = Vec::new();
        loop {
            match stream.next_event().await {
                Ok(StreamItem::Batch(batch)) => {
                    for row in 0..batch.len() {
                        if let CellRef::Text(key) = batch.cell(row, 1) {
                            keys.push(key.to_owned());
                        }
                    }
                }
                Ok(StreamItem::Done(outcome)) => return (keys, outcome),
                Err(error) => panic!("the listing failed: {error:?}"),
            }
        }
    }
}

#[cfg(test)]
#[allow(
    clippy::wildcard_enum_match_arm,
    reason = "a test's catch-all arm only prints the unexpected error"
)]
mod tests;
