//! The broker-neutral message API: where a read starts and which way it goes, the page it
//! answers, and the engine-free rules every engine applies before any round trip.

use std::time::Duration;

use crate::session::TruncatedBy;
use crate::value::{ColumnInfo, ResultBatch};

/// A message source: on Redis a DB index and a stream key.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceRef {
    /// The DB to read from; `None` means the session's own DB.
    pub db: Option<u32>,
    /// The source's name, which may not be UTF-8.
    pub name: Vec<u8>,
}

impl SourceRef {
    /// The stream `key` in Redis DB `db`.
    pub fn redis(db: u32, key: impl Into<Vec<u8>>) -> Self {
        SourceRef {
            db: Some(db),
            name: key.into(),
        }
    }
}

/// An opaque place in a source, as a page hands it back.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageCursor(CursorAt);

/// What a cursor holds, per engine.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CursorAt {
    /// A Redis stream entry id.
    RedisId(String),
}

impl MessageCursor {
    /// The Redis stream entry `id`.
    pub fn redis_id(id: impl Into<String>) -> Self {
        MessageCursor(CursorAt::RedisId(id.into()))
    }

    /// The Redis entry id this cursor names, if it names one.
    pub fn as_redis_id(&self) -> Option<&str> {
        match &self.0 {
            CursorAt::RedisId(id) => Some(id),
        }
    }
}

/// Where a read starts.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Position {
    /// The oldest message.
    Start,
    /// The newest message.
    End,
    /// Just after the cursor.
    After(MessageCursor),
    /// Just before the cursor.
    Before(MessageCursor),
}

/// Which way a read goes.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// Oldest to newest.
    Forward,
    /// Newest to oldest.
    Backward,
}

/// One read of a source: a browse page, or a follow when `wait` is set.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct MessageRead {
    /// What to read.
    pub source: SourceRef,
    /// Where the read starts.
    pub from: Position,
    /// Which way it goes.
    pub direction: Direction,
    /// How many messages the page aims for, clamped to 1..=10,000.
    pub want: u32,
    /// A bound on the page's cell bytes.
    pub max_bytes: Option<u64>,
    /// `None` browses; `Some` follows, waiting this long for new messages.
    pub wait: Option<Duration>,
}

impl MessageRead {
    /// A browse page of 100 messages with no byte bound.
    pub fn browse(source: SourceRef, from: Position, direction: Direction) -> Self {
        MessageRead {
            source,
            from,
            direction,
            want: 100,
            max_bytes: None,
            wait: None,
        }
    }

    /// A forward follow of up to 100 messages that waits up to `wait` for the first.
    pub fn follow(source: SourceRef, from: Position, wait: Duration) -> Self {
        MessageRead {
            wait: Some(wait),
            ..MessageRead::browse(source, from, Direction::Forward)
        }
    }
}

/// The messages one read found.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct MessagePage {
    /// The page's columns.
    pub columns: Vec<ColumnInfo>,
    /// The page's rows.
    pub batch: ResultBatch,
    /// Where to resume forward or follow from.
    pub next: Option<MessageCursor>,
    /// Where to page backward from.
    pub prev: Option<MessageCursor>,
    /// The source's length, when it knows it.
    pub length: Option<u64>,
    /// The cursor fell off retention and the read restarted at the oldest message.
    pub gap: bool,
    /// Why the page holds fewer rows or bytes than it could.
    pub truncated_by: Option<TruncatedBy>,
}

/// A consumer group of a stream.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupInfo {
    /// The group's name.
    pub name: String,
    /// How many consumers it has.
    pub consumers: u64,
    /// How many messages were delivered and not acknowledged.
    pub pending: u64,
    /// The last message delivered to the group.
    pub last_delivered: MessageCursor,
    /// How many messages wait for delivery, when the server knows.
    pub lag: Option<u64>,
}

/// Why a message read could not answer.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum MessageError {
    /// The engine has no messages, or cannot read this way.
    #[error("unsupported: {reason}")]
    Unsupported {
        /// Why, for the user.
        reason: String,
    },
    /// The source does not exist.
    #[error("no such source")]
    NotFound,
    /// The server refused the call for lack of permission.
    #[error("permission denied: {message}")]
    Denied {
        /// The server's words.
        message: String,
    },
    /// The follow's wait is over what the session allows.
    #[error("the wait is over the {max:?} the session allows")]
    WaitTooLong {
        /// The longest wait allowed; zero when the statement timeout leaves no room.
        max: Duration,
    },
    /// The server answered with an error.
    #[error("{message} ({code})")]
    Server {
        /// The server's error code, such as `WRONGTYPE`.
        code: String,
        /// The server's words after the code.
        message: String,
    },
    /// The statement timeout passed; the session has ended.
    #[error("the read timed out; reopen the session")]
    Timeout,
    /// The session ended, now or earlier.
    #[error("the session is closed")]
    SessionClosed,
    /// The read was cancelled; the session has ended.
    #[error("the read was cancelled; reopen the session")]
    Cancelled,
}

/// The most messages one page aims for.
pub(crate) const MAX_WANT: u32 = 10_000;

/// The longest follow wait with no statement timeout.
const MAX_WAIT: Duration = Duration::from_secs(5);

/// What a statement timeout keeps back from the wait, for the reply's round trip.
const WAIT_MARGIN: Duration = Duration::from_millis(250);

/// A read the engine may run, with its `want` clamped.
#[allow(dead_code)] // the stream reads that use the fields come next
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ReadPlan {
    /// A page from where the read starts, either way.
    Browse { want: u32 },
    /// A forward wait of `block_ms` for new messages.
    Follow { want: u32, block_ms: u64 },
}

/// Checks the read's position and direction pair, clamps `want` and, for a follow, the wait.
#[cfg_attr(not(feature = "redis"), allow(dead_code))]
pub(crate) fn plan_read(
    r: &MessageRead,
    timeout: Option<Duration>,
) -> Result<ReadPlan, MessageError> {
    let want = r.want.clamp(1, MAX_WANT);
    let unsupported = |reason: &str| {
        Err(MessageError::Unsupported {
            reason: reason.to_owned(),
        })
    };
    match r.wait {
        None => match (&r.from, r.direction) {
            (Position::Before(_), Direction::Forward) => {
                unsupported("a forward page starts at the start, the end or after a cursor")
            }
            (Position::After(_), Direction::Backward) => {
                unsupported("a backward page starts at the start, the end or before a cursor")
            }
            (Position::Start | Position::End | Position::After(_), Direction::Forward)
            | (Position::Start | Position::End | Position::Before(_), Direction::Backward) => {
                Ok(ReadPlan::Browse { want })
            }
        },
        Some(wait) => match (&r.from, r.direction) {
            (Position::After(_) | Position::End, Direction::Forward) => Ok(ReadPlan::Follow {
                want,
                block_ms: block_ms(wait, timeout)?,
            }),
            (Position::Start | Position::Before(_), Direction::Forward)
            | (
                Position::Start | Position::End | Position::After(_) | Position::Before(_),
                Direction::Backward,
            ) => unsupported("a follow reads forward from the end or after a cursor"),
        },
    }
}

/// The follow wait in whole milliseconds, rounded up and never zero, since `BLOCK 0` waits
/// forever. The cap is five seconds, or the statement timeout less a margin when that is less.
#[cfg_attr(not(feature = "redis"), allow(dead_code))]
pub(crate) fn block_ms(wait: Duration, timeout: Option<Duration>) -> Result<u64, MessageError> {
    let allowed = timeout.map_or(MAX_WAIT, |t| MAX_WAIT.min(t.saturating_sub(WAIT_MARGIN)));
    if allowed.is_zero() || wait > allowed {
        return Err(MessageError::WaitTooLong { max: allowed });
    }
    let ms = wait.as_nanos().div_ceil(1_000_000).max(1);
    Ok(u64::try_from(ms).unwrap_or(u64::MAX))
}

#[cfg(test)]
mod tests;
