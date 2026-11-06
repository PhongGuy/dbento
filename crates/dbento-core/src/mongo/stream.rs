//! A statement's rows: documents buffered to fix the columns, then the rest of the server cursor,
//! under the row and byte limits. Dropping the cursor sends `killCursors`.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::Ordering;

use bson::{Bson, Document, RawDocumentBuf};
use mongodb::action::Action;
use mongodb::{Cursor, Database};

use super::columns::ColumnModel;
use super::driver::{MongoCancel, Shared};
use super::exec::read_preference;
use super::parse::{MongoMethod, MongoOp};
use crate::session::{
    ExecuteError, ExtendError, Limits, OnTruncate, Outcome, StreamItem, TruncatedBy, Writes,
};
use crate::value::{BatchBuilder, ColumnInfo, MAX_BATCH_BYTES};

/// The server's default first batch.
const FIRST_BATCH: usize = 101;

pub(crate) struct MongoStream {
    model: Option<ColumnModel>,
    buffer: VecDeque<RawDocumentBuf>,
    cursor: Option<Cursor<RawDocumentBuf>>,
    /// The document that proved a parked row cut; it goes first once `extend` raises the cap.
    peeked: Option<RawDocumentBuf>,
    builder: BatchBuilder,
    limits: Limits,
    delivered: u64,
    done: Option<Outcome>,
    shared: Arc<Shared>,
    cancel: Arc<MongoCancel>,
    writes: Writes,
}

/// Whether the server answers `op` with a cursor. It answers an explain with a plain reply.
pub(crate) fn is_cursor(op: &MongoOp) -> bool {
    use MongoMethod::*;
    let command = match op {
        MongoOp::Use(_) => return false,
        MongoOp::Method { method, command } => {
            let listed = matches!(
                method,
                Find | FindOne | Aggregate | GetIndexes | GetCollectionNames
            );
            listed.then_some(command)
        }
        MongoOp::RunCommand(command) => {
            let first = command.keys().next().map(String::as_str);
            let listed = matches!(
                first,
                Some("find" | "aggregate" | "listCollections" | "listIndexes")
            );
            listed.then_some(command)
        }
    };
    command.is_some_and(|c| !c.contains_key("explain"))
}

/// `command` asking for no more than the limits can use, when a row limit sits below the first
/// batch and the user set no `batchSize`. An aggregate without `cursor` stays as the server's
/// own refusal; only `batchSize` is ever added, never a `limit`.
pub(crate) fn with_batch_size(mut command: Document, limits: Limits) -> Document {
    let wanted = limits.max_rows.map(|rows| rows.saturating_add(1));
    let Some(size) = wanted.filter(|&n| n < FIRST_BATCH as u64) else {
        return command;
    };
    let size = Bson::Int32(size as i32);
    match command.keys().next().map(String::as_str) {
        // On `singleBatch` a `batchSize` would cap the whole result, not its first reply.
        Some("find")
            if !command.contains_key("batchSize") && !command.contains_key("singleBatch") =>
        {
            command.insert("batchSize", size);
        }
        Some("aggregate") => {
            if let Ok(cursor) = command.get_document_mut("cursor")
                && !cursor.contains_key("batchSize")
            {
                cursor.insert("batchSize", size);
            }
        }
        _ => {}
    }
    command
}

/// Reads the documents the columns come from; the cursor comes back unless it ended inside them.
/// A cursor that ends just as the buffer fills still comes back: only a further `advance` could
/// tell, because `Cursor::has_next` is true while the current document sits in its batch.
async fn fill(
    mut cursor: Cursor<RawDocumentBuf>,
    limits: Limits,
) -> mongodb::error::Result<(Vec<RawDocumentBuf>, Option<Cursor<RawDocumentBuf>>)> {
    let rows = limits
        .max_rows
        .map_or(FIRST_BATCH as u64, |r| r.saturating_add(1))
        .min(FIRST_BATCH as u64) as usize;
    let cap = limits
        .max_bytes
        .map_or(MAX_BATCH_BYTES, |b| b.min(MAX_BATCH_BYTES as u64) as usize);
    let mut buffer = Vec::new();
    let mut bytes = 0;
    while cursor.advance().await? {
        let raw = cursor.current().to_owned();
        bytes += raw.as_bytes().len();
        buffer.push(raw);
        if buffer.len() >= rows || bytes >= cap {
            return Ok((buffer, Some(cursor)));
        }
    }
    Ok((buffer, None))
}

/// Opens `command`'s cursor where a statement of this kind belongs.
pub(super) async fn cursor_of(
    database: &Database,
    command: Document,
    writes: Writes,
) -> mongodb::error::Result<Cursor<Document>> {
    database
        .run_cursor_command(command)
        .optional(read_preference(database, writes), |call, criteria| {
            call.selection_criteria(criteria)
        })
        .await
}

/// `countDocuments`' `$group` document, left raw for `render::decode`. There is none at all
/// when nothing matched.
pub(crate) async fn counted(
    database: &Database,
    command: Document,
    writes: Writes,
) -> mongodb::error::Result<Option<RawDocumentBuf>> {
    let mut cursor = cursor_of(database, command, writes).await?;
    Ok(if cursor.advance().await? {
        Some(cursor.current().to_owned())
    } else {
        None
    })
}

fn outcome(rows: u64, cut: Option<TruncatedBy>, rows_affected: Option<u64>) -> Outcome {
    Outcome {
        rows,
        truncated: cut.is_some(),
        rows_affected,
        truncated_by: cut,
        extra_results: 0,
        rolled_back: false,
    }
}

impl MongoStream {
    /// A statement with no rows to stream, whether it sent nothing or only wrote.
    pub(crate) fn finished(
        rows_affected: Option<u64>,
        shared: Arc<Shared>,
        cancel: Arc<MongoCancel>,
    ) -> Self {
        MongoStream {
            model: None,
            buffer: VecDeque::new(),
            cursor: None,
            peeked: None,
            builder: BatchBuilder::new(0),
            limits: Limits::default(),
            delivered: 0,
            done: Some(outcome(0, None, rows_affected)),
            shared,
            cancel,
            writes: Writes::No,
        }
    }

    /// Sends `command` and buffers its first documents, which fix the columns.
    pub(crate) async fn open(
        database: &Database,
        command: Document,
        limits: Limits,
        writes: Writes,
        shared: Arc<Shared>,
        cancel: Arc<MongoCancel>,
    ) -> Result<(Vec<ColumnInfo>, Self), ExecuteError> {
        let first = async {
            let cursor = cursor_of(database, command, writes).await?;
            fill(cursor.with_type(), limits).await
        };
        let (buffer, cursor) = cancel
            .guard(shared.deadline(first))
            .await??
            .map_err(|e| shared.failure(e, writes))?;
        Self::buffered(buffer, cursor, limits, writes, shared, cancel)
    }

    /// A reply shown as rows: `document`, or no row when the reply holds none. Its columns are
    /// final, since nothing follows it.
    pub(crate) fn reply(
        document: Option<RawDocumentBuf>,
        limits: Limits,
        writes: Writes,
        shared: Arc<Shared>,
        cancel: Arc<MongoCancel>,
    ) -> Result<(Vec<ColumnInfo>, Self), ExecuteError> {
        let buffer = document.into_iter().collect();
        Self::buffered(buffer, None, limits, writes, shared, cancel)
    }

    fn buffered(
        buffer: Vec<RawDocumentBuf>,
        cursor: Option<Cursor<RawDocumentBuf>>,
        limits: Limits,
        writes: Writes,
        shared: Arc<Shared>,
        cancel: Arc<MongoCancel>,
    ) -> Result<(Vec<ColumnInfo>, Self), ExecuteError> {
        let (model, columns) = ColumnModel::of(&buffer, cursor.is_none())?;
        let stream = MongoStream {
            model: Some(model),
            buffer: buffer.into(),
            cursor,
            peeked: None,
            builder: BatchBuilder::new(columns.len()),
            limits,
            delivered: 0,
            done: None,
            shared,
            cancel,
            writes,
        };
        Ok((columns, stream))
    }

    /// The next batch or the end. A failure leaves no server cursor behind.
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
            let Some(raw) = self.next_document().await? else {
                self.done = Some(outcome(self.delivered, None, None));
                continue;
            };
            // A document past a reached bound proves the result was cut short.
            let cut = self.limits.cut(self.delivered, self.builder.total_bytes());
            if cut.is_some() {
                if cut == Some(TruncatedBy::Rows) && self.limits.on_truncate == OnTruncate::Park {
                    self.peeked = Some(raw);
                } else {
                    self.stop();
                }
                if self.limits.on_truncate == OnTruncate::Close {
                    self.shared.closed.store(true, Ordering::Relaxed);
                }
                self.done = Some(outcome(self.delivered, cut, None));
                continue;
            }
            let model = self.model.as_ref().ok_or(ExecuteError::SessionClosed)?;
            let closed = model.push(&mut self.builder, &raw)?;
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

    /// Raises a parked stream's row cap, or lifts it for `None`. Only a row cut parks: a stream
    /// that ended or was cut by bytes holds no document to go on from.
    pub(crate) fn extend(&mut self, more: Option<u64>) -> Result<(), ExtendError> {
        if self.peeked.is_none() {
            return Err(ExtendError::NotParked);
        }
        self.limits.max_rows = more.and_then(|m| self.limits.max_rows.map(|r| r.saturating_add(m)));
        self.done = None;
        Ok(())
    }

    async fn next_document(&mut self) -> Result<Option<RawDocumentBuf>, ExecuteError> {
        if let Some(raw) = self.peeked.take().or_else(|| self.buffer.pop_front()) {
            return Ok(Some(raw));
        }
        let Some(cursor) = &mut self.cursor else {
            return Ok(None);
        };
        let more = self
            .cancel
            .guard(self.shared.deadline(cursor.advance()))
            .await??
            .map_err(|e| expired(self.shared.failure(e, self.writes)))?;
        if more {
            return Ok(Some(cursor.current().to_owned()));
        }
        self.cursor = None;
        Ok(None)
    }

    fn stop(&mut self) {
        self.cursor = None;
        self.peeked = None;
        self.buffer.clear();
    }
}

// The server forgets an idle cursor on its own, and the stream cannot pick up where it was.
fn expired(mut failure: ExecuteError) -> ExecuteError {
    if let ExecuteError::Database { code, hint, .. } = &mut failure
        && code == "CursorNotFound"
    {
        *hint = Some("the cursor expired; run the query again".to_owned());
    }
    failure
}

#[cfg(test)]
mod tests;
