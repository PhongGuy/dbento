//! The message API over Redis Streams.

use ::redis::{Pipeline, Value};

use crate::message::{
    MessageCursor, MessageError, MessagePage, MessageRead, Position, ReadPlan, plan_read,
};
use crate::redis::browse::{KeyBrowseError, Layout, Row, bulk, columns, fill_within, xrange_rows};
use crate::redis::driver::{BracketError, Failure, RedisSession};
use crate::session::TruncatedBy;

/// The columns of every message page; `time` is the id's millisecond part.
const MESSAGE: Layout = &[
    ("id", "string", false),
    ("time", "integer", true),
    ("fields", "map", false),
];

impl RedisSession {
    /// One page of a stream, browsed either way or followed forward.
    pub(crate) async fn read_messages(&self, r: &MessageRead) -> Result<MessagePage, MessageError> {
        match plan_read(r, self.shared.timeout())? {
            ReadPlan::Follow { want, block_ms } => self.follow(r, want, block_ms).await,
            ReadPlan::Browse { .. } => Err(MessageError::Unsupported {
                reason: "stream reads are not built yet".to_owned(),
            }),
        }
    }

    /// A follow in two round trips: `TYPE` (and the newest id when starting at the end), then
    /// `XREAD BLOCK` with `XLEN`, which the server answers once the wait ends.
    async fn follow(
        &self,
        r: &MessageRead,
        want: u32,
        block_ms: u64,
    ) -> Result<MessagePage, MessageError> {
        let db = match r.source.db {
            Some(db) => db,
            None => self
                .shared
                .bracket_db()
                .map_err(|refusal| from_bracket(BracketError::Refused(refusal)))?,
        };
        let key = r.source.name.as_slice();
        let mut pipe = Pipeline::new();
        pipe.cmd("TYPE").arg(key);
        if r.from == Position::End {
            pipe.cmd("XREVRANGE")
                .arg(key)
                .arg("+")
                .arg("-")
                .arg("COUNT")
                .arg(1);
        }
        let mut replies = self.bracket(db, pipe).await?.into_iter();
        stream_type(replies.next())?;
        let start = match &r.from {
            Position::After(cursor) => cursor_id(cursor)?.to_owned(),
            Position::End => {
                let newest = xrange_rows(checked(replies.next())?).map_err(from_browse)?;
                match newest
                    .into_iter()
                    .next()
                    .and_then(|row| row.into_iter().next())
                {
                    Some(id) => String::from_utf8_lossy(&id).into_owned(),
                    None => "0-0".to_owned(),
                }
            }
            Position::Start | Position::Before(_) => {
                return Err(MessageError::Unsupported {
                    reason: "a follow reads forward from the end or after a cursor".to_owned(),
                });
            }
        };

        let mut pipe = Pipeline::new();
        pipe.cmd("XREAD")
            .arg("COUNT")
            .arg(want)
            .arg("BLOCK")
            .arg(block_ms)
            .arg("STREAMS")
            .arg(key)
            .arg(&start);
        pipe.cmd("XLEN").arg(key);
        let mut replies = self.bracket(db, pipe).await?.into_iter();
        let entries = xread_entries(checked(replies.next())?)?;
        #[allow(
            clippy::wildcard_enum_match_arm,
            reason = "Value has too many variants to list; the rest share one arm"
        )]
        let length = match checked(replies.next())? {
            Value::Int(n) => u64::try_from(n).unwrap_or(0),
            _ => return Err(unexpected("XLEN")),
        };
        let rows = xrange_rows(Value::Array(entries))
            .map_err(from_browse)?
            .into_iter()
            .map(timed_row)
            .collect::<Vec<Row>>();
        let budget = r
            .max_bytes
            .map_or(usize::MAX, |b| usize::try_from(b).unwrap_or(usize::MAX));
        let (batch, shown) = fill_within(&rows, rows.len(), MESSAGE, budget);
        let id_at = |i: usize| {
            rows.get(i)
                .and_then(|row| row.first())
                .map(|id| MessageCursor::redis_id(String::from_utf8_lossy(id)))
        };
        Ok(MessagePage {
            columns: columns(MESSAGE),
            batch,
            next: shown
                .checked_sub(1)
                .and_then(id_at)
                .or_else(|| Some(MessageCursor::redis_id(start))),
            prev: if shown > 0 { id_at(0) } else { None },
            length: Some(length),
            gap: false,
            truncated_by: (shown < rows.len()).then_some(TruncatedBy::Bytes),
        })
    }

    async fn bracket(&self, db: u32, pipe: Pipeline) -> Result<Vec<Value>, MessageError> {
        self.shared.bracket(db, pipe).await.map_err(from_bracket)
    }
}

/// A `TYPE` reply that names a stream; `none` is a missing key.
fn stream_type(reply: Option<Value>) -> Result<(), MessageError> {
    let name = bulk(checked(reply)?, "TYPE").map_err(from_browse)?;
    match name.as_slice() {
        b"stream" => Ok(()),
        b"none" => Err(MessageError::NotFound),
        other => Err(MessageError::Server {
            code: "WRONGTYPE".to_owned(),
            message: format!(
                "the key is a {}, not a stream",
                String::from_utf8_lossy(other)
            ),
        }),
    }
}

fn cursor_id(cursor: &MessageCursor) -> Result<&str, MessageError> {
    cursor
        .as_redis_id()
        .ok_or_else(|| MessageError::Unsupported {
            reason: "the cursor is not a Redis stream id".to_owned(),
        })
}

/// `[id, fields]` with the id's millisecond part inserted as `time`.
fn timed_row(mut row: Row) -> Row {
    let time = row.first().map_or_else(Vec::new, |id| {
        id.split(|b| *b == b'-').next().unwrap_or_default().to_vec()
    });
    row.insert(1, time);
    row
}

/// One inner reply of a bracket, or the error it carries.
fn checked(reply: Option<Value>) -> Result<Value, MessageError> {
    let reply = reply.ok_or_else(|| unexpected("pipeline"))?;
    BracketError::check(reply).map_err(from_bracket)
}

fn unexpected(what: &str) -> MessageError {
    MessageError::Server {
        code: "ERR".to_owned(),
        message: format!("unexpected {what} reply"),
    }
}

/// The entries of an `XREAD` on one key: RESP3 answers `{key: entries}`, RESP2
/// `[[key, entries]]`, and a wait that timed out answers nil.
pub(super) fn xread_entries(reply: Value) -> Result<Vec<Value>, MessageError> {
    let bad = || unexpected("XREAD");
    #[allow(
        clippy::wildcard_enum_match_arm,
        reason = "Value has too many variants to list; the rest share one arm"
    )]
    let entries = match reply {
        Value::Nil => return Ok(Vec::new()),
        Value::Array(items) if items.is_empty() => return Ok(Vec::new()),
        Value::Map(mut pairs) if pairs.len() == 1 => pairs.remove(0).1,
        Value::Array(mut items) if items.len() == 1 => match items.remove(0) {
            Value::Array(mut kv) if kv.len() == 2 => kv.remove(1),
            _ => return Err(bad()),
        },
        _ => return Err(bad()),
    };
    #[allow(
        clippy::wildcard_enum_match_arm,
        reason = "Value has too many variants to list; the rest share one arm"
    )]
    match entries {
        Value::Array(entries) => Ok(entries),
        _ => Err(bad()),
    }
}

/// A failed round trip: each failure ends the session in its own way.
pub(super) fn from_failure(f: Failure) -> MessageError {
    match f {
        Failure::Ended(_) => MessageError::SessionClosed,
        Failure::Timeout => MessageError::Timeout,
        Failure::Cancelled => MessageError::Cancelled,
    }
}

/// `NOPERM` is a denial; every other error reply is the server's.
fn from_bracket(e: BracketError) -> MessageError {
    match e {
        BracketError::Refused(refusal) => MessageError::Unsupported {
            reason: refusal.reason().to_owned(),
        },
        BracketError::Failed(failure) => from_failure(failure),
        BracketError::Reply { code, message } if code == "NOPERM" => {
            MessageError::Denied { message }
        }
        BracketError::Reply { code, message } => MessageError::Server { code, message },
    }
}

/// The browse helpers fail only on reply shapes, as `Database`.
fn from_browse(e: KeyBrowseError) -> MessageError {
    match e {
        KeyBrowseError::Database { code, message } => MessageError::Server { code, message },
        KeyBrowseError::Unsupported { reason } => MessageError::Unsupported { reason },
        KeyBrowseError::ReadOnlyViolation { message } => MessageError::Denied { message },
        KeyBrowseError::Timeout => MessageError::Timeout,
        KeyBrowseError::SessionEnded { .. } => MessageError::SessionClosed,
    }
}

#[cfg(test)]
mod tests;
