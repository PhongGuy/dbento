//! The message API over Redis Streams.

use ::redis::{Pipeline, Value};

use crate::message::{
    Direction, GroupInfo, MessageCursor, MessageError, MessagePage, MessageRead, Position,
    ReadPlan, SourceRef, plan_read,
};
use crate::redis::browse::{
    KeyBrowseError, Layout, Row, VALUE_CAP, bulk, columns, fill_within, stream_range, whole_chars,
    xrange_rows,
};
use crate::redis::driver::{BracketError, Failure, RedisSession, text_of};
use crate::session::TruncatedBy;

/// The columns of every message page; `time` is the id's millisecond part.
const MESSAGE: Layout = &[
    ("id", "string", false),
    ("time", "integer", true),
    ("fields", "map", false),
];

/// The columns of a pending page.
const PENDING: Layout = &[
    ("id", "string", false),
    ("consumer", "string", false),
    ("idle_ms", "integer", true),
    ("deliveries", "integer", true),
];

impl RedisSession {
    /// One page of a stream, browsed either way or followed forward.
    pub(crate) async fn read_messages(&self, r: &MessageRead) -> Result<MessagePage, MessageError> {
        match plan_read(r, self.shared.timeout())? {
            ReadPlan::Follow { want, block_ms } => self.follow(r, want, block_ms).await,
            ReadPlan::Browse { want } => self.browse(r, want).await,
        }
    }

    /// A browse in one round trip: `TYPE`, `XLEN`, then the range when the page can have rows.
    /// Forward from the end reads only the newest id, so the page can hand it on as `next`.
    async fn browse(&self, r: &MessageRead, want: u32) -> Result<MessagePage, MessageError> {
        let db = self.db_of(&r.source)?;
        let key = r.source.name.as_slice();
        let bound = match &r.from {
            Position::After(cursor) | Position::Before(cursor) => Some(cursor_id(cursor)?),
            Position::Start | Position::End => None,
        };
        let mut pipe = Pipeline::new();
        pipe.cmd("TYPE").arg(key);
        pipe.cmd("XLEN").arg(key);
        match (&r.from, r.direction) {
            (Position::End, Direction::Forward) => {
                stream_range(&mut pipe, key, Direction::Backward, None, 1);
            }
            (Position::Start, Direction::Backward) => {}
            (Position::Start | Position::End | Position::After(_) | Position::Before(_), dir) => {
                stream_range(&mut pipe, key, dir, bound.map(str::as_bytes), want as usize);
            }
        }
        let mut replies = self.bracket(db, pipe).await?.into_iter();
        stream_type(replies.next())?;
        let length = xlen(replies.next())?;
        let (rows, cut, empty_next) = match (&r.from, r.direction) {
            (Position::End, Direction::Forward) => {
                let newest = newest_id(checked(replies.next())?)?;
                (Vec::new(), false, Some(newest))
            }
            (Position::Start, Direction::Backward) => (Vec::new(), false, None),
            (Position::Start | Position::End | Position::After(_) | Position::Before(_), _) => {
                let (rows, cut) = message_rows(checked(replies.next())?)?;
                let empty_next = bound.map_or_else(|| "0-0".to_owned(), str::to_owned);
                (rows, cut, Some(empty_next))
            }
        };
        Ok(page(r, rows, cut, empty_next, length))
    }

    /// A follow in two round trips: `TYPE` (and the newest id when starting at the end), then
    /// `XREAD BLOCK` with `XLEN`, which the server answers once the wait ends.
    async fn follow(
        &self,
        r: &MessageRead,
        want: u32,
        block_ms: u64,
    ) -> Result<MessagePage, MessageError> {
        let db = self.db_of(&r.source)?;
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
            Position::End => newest_id(checked(replies.next())?)?,
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
        let length = xlen(replies.next())?;
        let (rows, cut) = message_rows(Value::Array(entries))?;
        Ok(page(r, rows, cut, Some(start), length))
    }

    /// The consumer groups of a stream, in one round trip of `TYPE` and `XINFO GROUPS`.
    pub(crate) async fn stream_groups(
        &self,
        s: &SourceRef,
    ) -> Result<Vec<GroupInfo>, MessageError> {
        let db = self.db_of(s)?;
        let mut pipe = Pipeline::new();
        pipe.cmd("TYPE").arg(s.name.as_slice());
        pipe.cmd("XINFO").arg("GROUPS").arg(s.name.as_slice());
        let mut replies = self.bracket(db, pipe).await?.into_iter();
        stream_type(replies.next())?;
        group_infos(checked(replies.next())?)
    }

    /// One forward page of a group's pending entries in one round trip: `TYPE`, the `XPENDING`
    /// summary for the count, then the entries unless the page starts at the end. `from` and
    /// `want` follow a forward browse's rules.
    pub(crate) async fn pending(
        &self,
        s: &SourceRef,
        group: &str,
        from: Position,
        want: u32,
    ) -> Result<MessagePage, MessageError> {
        let read = MessageRead {
            want,
            ..MessageRead::browse(s.clone(), from, Direction::Forward)
        };
        let (ReadPlan::Browse { want } | ReadPlan::Follow { want, .. }) = plan_read(&read, None)?;
        let db = self.db_of(s)?;
        let key = s.name.as_slice();
        let start = match &read.from {
            Position::Start => Some("-".to_owned()),
            Position::After(cursor) => Some(format!("({}", cursor_id(cursor)?)),
            Position::End | Position::Before(_) => None,
        };
        let mut pipe = Pipeline::new();
        pipe.cmd("TYPE").arg(key);
        pipe.cmd("XPENDING").arg(key).arg(group);
        if let Some(start) = &start {
            pipe.cmd("XPENDING")
                .arg(key)
                .arg(group)
                .arg(start)
                .arg("+")
                .arg(want);
        }
        let mut replies = self.bracket(db, pipe).await?.into_iter();
        stream_type(replies.next())?;
        let length = pending_count(checked(replies.next())?)?;
        let rows = match start {
            Some(_) => pending_rows(checked(replies.next())?)?,
            None => Vec::new(),
        };
        let (batch, shown) = fill_within(&rows, rows.len(), PENDING, usize::MAX);
        let after = match &read.from {
            Position::After(cursor) => Some(cursor.clone()),
            Position::Start | Position::End | Position::Before(_) => None,
        };
        let next = shown
            .checked_sub(1)
            .and_then(|last| rows.get(last))
            .and_then(|row| row.first())
            .map(|id| MessageCursor::redis_id(String::from_utf8_lossy(id)))
            .or(after);
        Ok(MessagePage {
            columns: columns(PENDING),
            batch,
            next,
            prev: None,
            length: Some(length),
            gap: false,
            truncated_by: (shown < rows.len()).then_some(TruncatedBy::Bytes),
        })
    }

    /// The database a read brackets: its own, or the session's.
    fn db_of(&self, source: &SourceRef) -> Result<u32, MessageError> {
        match source.db {
            Some(db) => Ok(db),
            None => self
                .shared
                .bracket_db()
                .map_err(|refusal| from_bracket(BracketError::Refused(refusal))),
        }
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

/// Shows the rows `max_bytes` lets through. A page reads its cursors off the rows shown, in
/// its own direction; an empty forward page keeps `empty_next` so the reader can go on.
fn page(
    r: &MessageRead,
    rows: Vec<Row>,
    cut: bool,
    empty_next: Option<String>,
    length: u64,
) -> MessagePage {
    let budget = r
        .max_bytes
        .map_or(usize::MAX, |b| usize::try_from(b).unwrap_or(usize::MAX));
    let (batch, shown) = fill_within(&rows, rows.len(), MESSAGE, budget);
    let id_at = |i: usize| {
        rows.get(i)
            .and_then(|row| row.first())
            .map(|id| MessageCursor::redis_id(String::from_utf8_lossy(id)))
    };
    let (first, last) = match shown {
        0 => (None, None),
        n => (id_at(0), id_at(n - 1)),
    };
    let (next, prev) = match r.direction {
        Direction::Forward => (
            last.or_else(|| empty_next.map(MessageCursor::redis_id)),
            first,
        ),
        Direction::Backward => (first, last),
    };
    MessagePage {
        columns: columns(MESSAGE),
        batch,
        next,
        prev,
        length: Some(length),
        gap: false,
        truncated_by: (cut || shown < rows.len()).then_some(TruncatedBy::Bytes),
    }
}

/// Range entries as message rows, with any `fields` cell over `VALUE_CAP` cut to whole chars;
/// the flag says whether a cell was cut.
fn message_rows(reply: Value) -> Result<(Vec<Row>, bool), MessageError> {
    let mut cut = false;
    let rows = xrange_rows(reply)
        .map_err(from_browse)?
        .into_iter()
        .map(|mut row| {
            if let Some(fields) = row.get_mut(1)
                && fields.len() > VALUE_CAP
            {
                fields.truncate(VALUE_CAP);
                *fields = whole_chars(std::mem::take(fields), true);
                cut = true;
            }
            timed_row(row)
        })
        .collect();
    Ok((rows, cut))
}

/// The id of the one entry an `XREVRANGE .. COUNT 1` answers, or `0-0` for an empty stream.
fn newest_id(reply: Value) -> Result<String, MessageError> {
    let newest = xrange_rows(reply).map_err(from_browse)?;
    Ok(newest
        .into_iter()
        .next()
        .and_then(|row| row.into_iter().next())
        .map_or_else(
            || "0-0".to_owned(),
            |id| String::from_utf8_lossy(&id).into_owned(),
        ))
}

fn xlen(reply: Option<Value>) -> Result<u64, MessageError> {
    #[allow(
        clippy::wildcard_enum_match_arm,
        reason = "Value has too many variants to list; the rest share one arm"
    )]
    match checked(reply)? {
        Value::Int(n) => Ok(u64::try_from(n).unwrap_or(0)),
        _ => Err(unexpected("XLEN")),
    }
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

/// The groups of an `XINFO GROUPS` reply. Each group is read by field name, from a RESP3 map
/// or a flat RESP2 array, so a server adding fields does not break it; a nil `lag` is `None`.
pub(super) fn group_infos(reply: Value) -> Result<Vec<GroupInfo>, MessageError> {
    let bad = || unexpected("XINFO GROUPS");
    let int = |v: Option<&Value>| match v {
        Some(Value::Int(n)) => u64::try_from(*n).ok(),
        Some(_) | None => None,
    };
    each_item(reply, "XINFO GROUPS")?
        .into_iter()
        .map(|group| {
            let fields = field_pairs(group).ok_or_else(bad)?;
            let get = |name: &str| {
                fields
                    .iter()
                    .find(|(k, _)| text_of(k).as_deref() == Some(name))
                    .map(|(_, v)| v)
            };
            let text = |name: &str| get(name).and_then(text_of).ok_or_else(bad);
            Ok(GroupInfo {
                name: text("name")?,
                consumers: int(get("consumers")).ok_or_else(bad)?,
                pending: int(get("pending")).ok_or_else(bad)?,
                last_delivered: MessageCursor::redis_id(text("last-delivered-id")?),
                lag: int(get("lag")),
            })
        })
        .collect()
}

/// The entry count of an `XPENDING` summary, `[count, min, max, consumers]`.
fn pending_count(reply: Value) -> Result<u64, MessageError> {
    #[allow(
        clippy::wildcard_enum_match_arm,
        reason = "Value has too many variants to list; the rest share one arm"
    )]
    match each_item(reply, "XPENDING")?.first() {
        Some(Value::Int(n)) => Ok(u64::try_from(*n).unwrap_or(0)),
        _ => Err(unexpected("XPENDING")),
    }
}

/// The rows of an extended `XPENDING`: `[id, consumer, idle ms, deliveries]`, the numbers as
/// their decimal text.
#[allow(
    clippy::wildcard_enum_match_arm,
    reason = "Value has too many variants to list; the rest share one arm"
)]
pub(super) fn pending_rows(reply: Value) -> Result<Vec<Row>, MessageError> {
    let bad = || unexpected("XPENDING");
    each_item(reply, "XPENDING")?
        .into_iter()
        .map(|entry| {
            let cell = |v: &Value| match v {
                Value::Int(n) => Some(n.to_string().into_bytes()),
                other => text_of(other).map(String::into_bytes),
            };
            match entry {
                Value::Array(parts) if parts.len() == 4 => parts
                    .iter()
                    .map(cell)
                    .collect::<Option<Row>>()
                    .ok_or_else(bad),
                _ => Err(bad()),
            }
        })
        .collect()
}

/// The items of an array reply; nil reads as none.
fn each_item(reply: Value, what: &str) -> Result<Vec<Value>, MessageError> {
    #[allow(
        clippy::wildcard_enum_match_arm,
        reason = "Value has too many variants to list; the rest share one arm"
    )]
    match reply {
        Value::Array(items) | Value::Set(items) => Ok(items),
        Value::Nil => Ok(Vec::new()),
        _ => Err(unexpected(what)),
    }
}

/// A RESP3 map as its pairs, or a flat `[k, v, ...]` array paired up.
fn field_pairs(value: Value) -> Option<Vec<(Value, Value)>> {
    #[allow(
        clippy::wildcard_enum_match_arm,
        reason = "Value has too many variants to list; the rest share one arm"
    )]
    match value {
        Value::Map(pairs) => Some(pairs),
        Value::Array(flat) if flat.len() % 2 == 0 => {
            let mut items = flat.into_iter();
            let mut pairs = Vec::new();
            while let (Some(k), Some(v)) = (items.next(), items.next()) {
                pairs.push((k, v));
            }
            Some(pairs)
        }
        _ => None,
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
