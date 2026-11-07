//! The key browser: its public types in every build, and with the `redis` feature the `SCAN`
//! walk that fills a page with each key's type and time to live.

use std::collections::HashSet;
use std::time::Duration;

/// One page request of the key browser.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct KeyScan {
    /// The DB index to browse; the session's own DB is left where it was.
    pub db: u32,
    /// A `SCAN MATCH` glob; `None` matches every key.
    pub pattern: Option<String>,
    /// Only keys of this kind; `None` takes every kind.
    pub kind: Option<KeyKind>,
    /// Where the walk resumes; `KeyCursor::start()` for the first page.
    pub cursor: KeyCursor,
    /// How many new keys the page aims for, clamped to 1..=10,000.
    pub want: u32,
}

impl KeyScan {
    /// The first page of `db`: no pattern, any kind, 100 keys.
    pub fn new(db: u32) -> Self {
        KeyScan {
            db,
            pattern: None,
            kind: None,
            cursor: KeyCursor::start(),
            want: 100,
        }
    }
}

/// The keys one page found, each once across the walk.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct KeyPage {
    /// May hold more than `want`: every new name of the page's round trips is kept.
    pub keys: Vec<KeyEntry>,
    /// Where the next page resumes; `None` once the walk is complete.
    pub next: Option<KeyCursor>,
}

/// A key, its kind and its time to live as read just after the scan.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyEntry {
    /// The key's name, which may not be UTF-8.
    pub name: Vec<u8>,
    /// What `TYPE` named it.
    pub kind: KeyKind,
    /// What `PTTL` answered.
    pub ttl: KeyTtl,
}

/// A key's kind, as the server's `TYPE` names it.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyKind {
    /// `string`.
    String,
    /// `hash`.
    Hash,
    /// `list`.
    List,
    /// `set`.
    Set,
    /// `zset`.
    SortedSet,
    /// `stream`.
    Stream,
    /// A module type, or `none` for a key that is gone.
    Other(String),
}

/// A key's time to live.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyTtl {
    /// The key never expires.
    Persistent,
    /// The key expires after this long.
    Expires(Duration),
    /// The key no longer exists.
    Gone,
}

/// Where a key walk stands: the server's cursor and every name already returned, since `SCAN`
/// may return a key more than once.
#[derive(Debug, Clone, Default)]
pub struct KeyCursor {
    server: u64,
    seen: HashSet<Vec<u8>>,
}

impl KeyCursor {
    /// The start of a walk.
    pub fn start() -> Self {
        KeyCursor::default()
    }
}

/// Why the key browser could not answer.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum KeyBrowseError {
    /// Not a Redis session, or one inside `MULTI`.
    #[error("unsupported: {reason}")]
    Unsupported {
        /// Why, for the user.
        reason: String,
    },
    /// The server refused a call, such as a `SELECT` of a DB it does not have.
    #[error("{message} ({code})")]
    Database {
        /// The server's error code, such as `ERR` or `NOPERM`.
        code: String,
        /// The server's words after the code.
        message: String,
    },
    /// The read-only session refused the call.
    #[error("read-only session: {message}")]
    ReadOnlyViolation {
        /// The server's words.
        message: String,
    },
    /// The statement timeout passed; the session has ended.
    #[error("the browse timed out")]
    Timeout,
    /// The session ended, now or earlier.
    #[error("the session is closed: {reason}")]
    SessionEnded {
        /// Why it ended.
        reason: String,
    },
}

#[cfg(feature = "redis")]
pub(super) use self::server::server_error;

#[cfg(feature = "redis")]
mod server {
    use std::time::Duration;

    use ::redis::{Pipeline, Value};

    use super::{KeyBrowseError, KeyEntry, KeyKind, KeyPage, KeyScan, KeyTtl};
    use crate::redis::driver::{Failure, RedisSession};

    /// The most keys one page aims for.
    const MAX_WANT: u32 = 10_000;
    /// A page answers after this many `SCAN` round trips, so a sparse pattern still answers.
    const MAX_ROUND_TRIPS: usize = 64;
    /// The `SCAN COUNT` hint per round trip.
    const SCAN_COUNT: u32 = 1_000;

    impl From<Failure> for KeyBrowseError {
        fn from(failure: Failure) -> Self {
            match failure {
                Failure::Ended(reason) => KeyBrowseError::SessionEnded {
                    reason: format!("{reason}; reopen the session"),
                },
                Failure::Timeout => KeyBrowseError::Timeout,
                Failure::Cancelled => KeyBrowseError::SessionEnded {
                    reason: "the browse was cancelled; reopen the session".to_owned(),
                },
            }
        }
    }

    impl KeyKind {
        fn from_type(name: &str) -> Self {
            match name {
                "string" => KeyKind::String,
                "hash" => KeyKind::Hash,
                "list" => KeyKind::List,
                "set" => KeyKind::Set,
                "zset" => KeyKind::SortedSet,
                "stream" => KeyKind::Stream,
                other => KeyKind::Other(other.to_owned()),
            }
        }

        fn type_name(&self) -> &str {
            match self {
                KeyKind::String => "string",
                KeyKind::Hash => "hash",
                KeyKind::List => "list",
                KeyKind::Set => "set",
                KeyKind::SortedSet => "zset",
                KeyKind::Stream => "stream",
                KeyKind::Other(name) => name,
            }
        }
    }

    impl KeyTtl {
        fn from_pttl(ms: i64) -> Self {
            match ms {
                -2 => KeyTtl::Gone,
                ms => u64::try_from(ms).map_or(KeyTtl::Persistent, |ms| {
                    KeyTtl::Expires(Duration::from_millis(ms))
                }),
            }
        }
    }

    impl RedisSession {
        /// One page of keys: `SCAN` round trips until `want` new names arrived, the walk ended
        /// or the round-trip budget ran out, then one pipeline of `TYPE` and `PTTL` for them.
        pub(crate) async fn scan_keys(&self, req: &KeyScan) -> Result<KeyPage, KeyBrowseError> {
            let shared = &self.shared;
            let want = req.want.clamp(1, MAX_WANT) as usize;
            let mut cursor = req.cursor.clone();
            let mut fresh: Vec<Vec<u8>> = Vec::new();
            let mut complete = false;
            for _ in 0..MAX_ROUND_TRIPS {
                let mut pipe = Pipeline::new();
                let scan = pipe
                    .cmd("SCAN")
                    .arg(cursor.server)
                    .arg("MATCH")
                    .arg(req.pattern.as_deref().unwrap_or("*"))
                    .arg("COUNT")
                    .arg(SCAN_COUNT);
                if let Some(kind) = &req.kind {
                    scan.arg("TYPE").arg(kind.type_name());
                }
                let reply = only(shared.bracketed(req.db, pipe).await?)?;
                let (next, names) = scan_reply(reply)?;
                cursor.server = next;
                fresh.extend(names.into_iter().filter(|n| cursor.seen.insert(n.clone())));
                if next == 0 {
                    complete = true;
                    break;
                }
                if fresh.len() >= want {
                    break;
                }
            }
            let keys = if fresh.is_empty() {
                Vec::new()
            } else {
                let mut pipe = Pipeline::new();
                for name in &fresh {
                    pipe.cmd("TYPE").arg(name.as_slice());
                    pipe.cmd("PTTL").arg(name.as_slice());
                }
                let replies = shared.bracketed(req.db, pipe).await?;
                entries(fresh, replies)?
            };
            Ok(KeyPage {
                keys,
                next: (!complete).then_some(cursor),
            })
        }
    }

    /// The one reply of a one-command pipeline.
    fn only(replies: Vec<Value>) -> Result<Value, KeyBrowseError> {
        let mut replies = replies.into_iter();
        match (replies.next(), replies.next()) {
            (Some(reply), None) => server_error(reply),
            _ => Err(unexpected("SCAN")),
        }
    }

    /// A reply, or the server error it carries as `Database`.
    pub(in crate::redis) fn server_error(reply: Value) -> Result<Value, KeyBrowseError> {
        match reply {
            Value::ServerError(e) => {
                let code = e.code().to_owned();
                // A bare code has no message, and the code alone then says what failed.
                let message = e.details().unwrap_or(&code).to_owned();
                Err(KeyBrowseError::Database { code, message })
            }
            other => Ok(other),
        }
    }

    fn unexpected(what: &str) -> KeyBrowseError {
        KeyBrowseError::Database {
            code: "ERR".to_owned(),
            message: format!("unexpected {what} reply"),
        }
    }

    /// `[cursor, [names]]`, the cursor sent as a decimal string.
    fn scan_reply(reply: Value) -> Result<(u64, Vec<Vec<u8>>), KeyBrowseError> {
        let Value::Array(mut parts) = reply else {
            return Err(unexpected("SCAN"));
        };
        let (Some(Value::Array(names)), Some(Value::BulkString(next)), None) =
            (parts.pop(), parts.pop(), parts.pop())
        else {
            return Err(unexpected("SCAN"));
        };
        let next = std::str::from_utf8(&next)
            .ok()
            .and_then(|n| n.parse::<u64>().ok())
            .ok_or_else(|| unexpected("SCAN"))?;
        let names = names
            .into_iter()
            .map(|n| match n {
                Value::BulkString(bytes) => Ok(bytes),
                _ => Err(unexpected("SCAN")),
            })
            .collect::<Result<_, _>>()?;
        Ok((next, names))
    }

    /// Pairs each name with its `TYPE` and `PTTL` replies, in the order they were sent.
    fn entries(names: Vec<Vec<u8>>, replies: Vec<Value>) -> Result<Vec<KeyEntry>, KeyBrowseError> {
        if replies.len() != names.len() * 2 {
            return Err(unexpected("TYPE and PTTL"));
        }
        let mut replies = replies.into_iter();
        names
            .into_iter()
            .map(|name| {
                let kind = match server_error(replies.next().unwrap_or(Value::Nil))? {
                    Value::SimpleString(t) => KeyKind::from_type(&t),
                    Value::BulkString(t) => KeyKind::from_type(&String::from_utf8_lossy(&t)),
                    _ => return Err(unexpected("TYPE")),
                };
                let ttl = match server_error(replies.next().unwrap_or(Value::Nil))? {
                    Value::Int(ms) => KeyTtl::from_pttl(ms),
                    _ => return Err(unexpected("PTTL")),
                };
                Ok(KeyEntry { name, kind, ttl })
            })
            .collect()
    }
}
