//! The key browser: its public types in every build, and with the `redis` feature the `SCAN`
//! walk that fills a page with each key's type and time to live, and the typed value pages.

use std::collections::{HashSet, VecDeque};
use std::time::Duration;

use crate::value::{ColumnInfo, ResultBatch};

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

/// One page request of a key's value.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct KeyRead {
    /// The DB index the key lives in; the session's own DB is left where it was.
    pub db: u32,
    /// The key's name, as raw bytes.
    pub key: Vec<u8>,
    /// Where the read resumes; `ValueCursor::start()` for the first page.
    pub from: ValueCursor,
    /// How many rows the page aims for, clamped to 1..=10,000.
    pub want: u32,
}

impl KeyRead {
    /// The first page of `key` in `db`, 100 rows.
    pub fn new(db: u32, key: Vec<u8>) -> Self {
        KeyRead {
            db,
            key,
            from: ValueCursor::start(),
            want: 100,
        }
    }
}

/// One page of a key's value, in the columns its kind reads as.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct KeyValuePage {
    /// What `TYPE` named the key; `Other("none")` for a key that is gone.
    pub kind: KeyKind,
    /// What `PTTL` answered.
    pub ttl: KeyTtl,
    /// The string's bytes or the aggregate's element count; `None` for a module type or a gone key.
    pub length: Option<u64>,
    /// The page's columns, such as `field` and `value` for a hash.
    pub columns: Vec<ColumnInfo>,
    /// The page's rows.
    pub batch: ResultBatch,
    /// Where the next page resumes; `None` once the value is read to its end.
    pub next: Option<ValueCursor>,
    /// A string longer than the 1 MiB page shows only its start.
    pub truncated_value: bool,
}

/// Where a value read stands: the kind it began on, the server cursor, offset or last stream id,
/// the hash fields or set members already returned, and rows read but not yet shown.
#[derive(Debug, Clone, Default)]
pub struct ValueCursor {
    kind: Option<KeyKind>,
    /// An `HSCAN` or `SSCAN` cursor.
    server: u64,
    /// The scan's cursor came back 0, so only `pending` is left.
    scanned: bool,
    /// The next `LRANGE` or `ZRANGE` index.
    offset: u64,
    /// The id of the last stream entry shown.
    last_id: Option<Vec<u8>>,
    seen: HashSet<Vec<u8>>,
    /// Scanned rows a page's byte bound held back, served before the next server call.
    pending: VecDeque<Vec<Vec<u8>>>,
}

impl ValueCursor {
    /// The start of a read.
    pub fn start() -> Self {
        ValueCursor::default()
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

#[cfg(all(test, feature = "redis"))]
use self::server::fields_json;
#[cfg(feature = "redis")]
pub(super) use self::server::server_error;

#[cfg(feature = "redis")]
mod server {
    use std::time::Duration;

    use ::redis::{Pipeline, Value};

    use super::{
        KeyBrowseError, KeyEntry, KeyKind, KeyPage, KeyRead, KeyScan, KeyTtl, KeyValuePage,
    };
    use crate::redis::driver::{Failure, RedisSession};
    use crate::redis::shape::{double_text, json, json_key};
    use crate::value::{BatchBuilder, CellRef, ColumnInfo, MAX_BATCH_BYTES, ResultBatch};

    /// The most of a string one value page shows.
    pub(crate) const VALUE_CAP: usize = 1 << 20;

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

    /// One page row as raw cell bytes; UTF-8 shows as text, anything else as bytes.
    type Row = Vec<Vec<u8>>;

    /// A kind's page columns: name, type name and whether its cells are numbers.
    type Layout = &'static [(&'static str, &'static str, bool)];

    const STRING: Layout = &[("value", "string", false)];
    const HASH: Layout = &[("field", "string", false), ("value", "string", false)];
    const SET: Layout = &[("member", "string", false)];
    const LIST: Layout = &[("index", "integer", true), ("value", "string", false)];
    const ZSET: Layout = &[("member", "string", false), ("score", "double", true)];
    const STREAM: Layout = &[("id", "string", false), ("fields", "map", false)];
    const MODULE: Layout = &[("type", "string", false), ("bytes", "integer", true)];

    impl RedisSession {
        /// One page of `req.key`'s value: `TYPE` and `PTTL`, then the length and the page in one
        /// pipeline. A key whose type changed in between is re-read once.
        pub(crate) async fn read_key(&self, req: &KeyRead) -> Result<KeyValuePage, KeyBrowseError> {
            let first = self.kind_and_ttl(req).await?;
            match self.value_page(req, first).await {
                Err(KeyBrowseError::Database { code, .. }) if code == "WRONGTYPE" => {
                    let again = self.kind_and_ttl(req).await?;
                    self.value_page(req, again).await
                }
                other => other,
            }
        }

        async fn kind_and_ttl(&self, req: &KeyRead) -> Result<KeyEntry, KeyBrowseError> {
            let mut pipe = Pipeline::new();
            pipe.cmd("TYPE").arg(req.key.as_slice());
            pipe.cmd("PTTL").arg(req.key.as_slice());
            let replies = self.shared.bracketed(req.db, pipe).await?;
            let mut entry = entries(vec![req.key.clone()], replies)?;
            entry.pop().ok_or_else(|| unexpected("TYPE"))
        }

        async fn value_page(
            &self,
            req: &KeyRead,
            key: KeyEntry,
        ) -> Result<KeyValuePage, KeyBrowseError> {
            let KeyEntry { kind, ttl, .. } = key;
            if kind == KeyKind::Other("none".to_owned()) {
                return Ok(gone());
            }
            if let Some(began) = &req.from.kind
                && *began != kind
            {
                return Err(KeyBrowseError::Database {
                    code: "WRONGTYPE".to_owned(),
                    message: format!(
                        "the key is now a {}, not a {}; read it again from the start",
                        kind.type_name(),
                        began.type_name()
                    ),
                });
            }
            let want = req.want.clamp(1, MAX_WANT) as usize;
            let mut cursor = req.from.clone();
            cursor.kind = Some(kind.clone());
            let k = req.key.as_slice();
            // A scan with rows held back serves them before it asks the server for more.
            let ask = cursor.pending.is_empty() && !cursor.scanned;
            let (start, end) = (cursor.offset, cursor.offset + want as u64 - 1);
            let mut pipe = Pipeline::new();
            match &kind {
                KeyKind::String => {
                    pipe.cmd("STRLEN").arg(k);
                    pipe.cmd("GETRANGE").arg(k).arg(0).arg(VALUE_CAP - 1);
                }
                KeyKind::Hash | KeyKind::Set => {
                    let (len, scan) = if kind == KeyKind::Hash {
                        ("HLEN", "HSCAN")
                    } else {
                        ("SCARD", "SSCAN")
                    };
                    pipe.cmd(len).arg(k);
                    if ask {
                        pipe.cmd(scan)
                            .arg(k)
                            .arg(cursor.server)
                            .arg("COUNT")
                            .arg(want);
                    }
                }
                KeyKind::List => {
                    pipe.cmd("LLEN").arg(k);
                    pipe.cmd("LRANGE").arg(k).arg(start).arg(end);
                }
                KeyKind::SortedSet => {
                    pipe.cmd("ZCARD").arg(k);
                    pipe.cmd("ZRANGE")
                        .arg(k)
                        .arg(start)
                        .arg(end)
                        .arg("WITHSCORES");
                }
                KeyKind::Stream => {
                    let from = match &cursor.last_id {
                        Some(id) => [b"(".as_slice(), id].concat(),
                        None => b"-".to_vec(),
                    };
                    pipe.cmd("XLEN").arg(k);
                    pipe.cmd("XRANGE")
                        .arg(k)
                        .arg(from)
                        .arg("+")
                        .arg("COUNT")
                        .arg(want);
                }
                KeyKind::Other(_) => {
                    pipe.cmd("MEMORY").arg("USAGE").arg(k);
                }
            }
            let mut replies = self
                .shared
                .bracketed(req.db, pipe)
                .await?
                .into_iter()
                .map(server_error);
            let mut next_reply = || replies.next().unwrap_or(Ok(Value::Nil));
            let length = match kind {
                KeyKind::Other(_) => None,
                _ => match next_reply()? {
                    Value::Int(n) => Some(u64::try_from(n).unwrap_or(0)),
                    _ => return Err(unexpected("length")),
                },
            };
            let page = |layout, batch, next, truncated_value| KeyValuePage {
                kind: kind.clone(),
                ttl,
                length,
                columns: columns(layout),
                batch,
                next,
                truncated_value,
            };
            match &kind {
                KeyKind::String => {
                    let truncated = length.is_some_and(|n| n > VALUE_CAP as u64);
                    let value = whole_chars(bulk(next_reply()?, "GETRANGE")?, truncated);
                    let (batch, _) = fill(&[vec![value]], 1, STRING);
                    Ok(page(STRING, batch, None, truncated))
                }
                KeyKind::Hash | KeyKind::Set => {
                    let layout = if kind == KeyKind::Hash { HASH } else { SET };
                    if ask {
                        let (server, flat) = scan_reply(next_reply()?)?;
                        cursor.server = server;
                        cursor.scanned = server == 0;
                        let mut flat = flat.into_iter();
                        while let Some(name) = flat.next() {
                            let mut row = vec![name];
                            row.extend(flat.by_ref().take(layout.len() - 1));
                            // `HSCAN` and `SSCAN` may return an element twice.
                            if row.len() == layout.len() && cursor.seen.insert(row[0].clone()) {
                                cursor.pending.push_back(row);
                            }
                        }
                    }
                    let rows = Vec::from(std::mem::take(&mut cursor.pending));
                    let (batch, shown) = fill(&rows, want, layout);
                    cursor.pending = rows.into_iter().skip(shown).collect();
                    let done = cursor.scanned && cursor.pending.is_empty();
                    Ok(page(layout, batch, (!done).then_some(cursor), false))
                }
                // Ranged kinds resume from the last row shown, so rows the byte bound held
                // back are fetched again rather than kept.
                KeyKind::List | KeyKind::SortedSet => {
                    let (layout, rows) = if kind == KeyKind::List {
                        let values = array(next_reply()?, "LRANGE")?;
                        let rows = values
                            .into_iter()
                            .zip(cursor.offset..)
                            .map(|(v, i)| Ok(vec![i.to_string().into_bytes(), bulk(v, "LRANGE")?]))
                            .collect::<Result<Vec<_>, KeyBrowseError>>()?;
                        (LIST, rows)
                    } else {
                        (ZSET, zrange_rows(next_reply()?)?)
                    };
                    let (batch, shown) = fill(&rows, want, layout);
                    cursor.offset += shown as u64;
                    let more = cursor.offset < length.unwrap_or(0);
                    Ok(page(layout, batch, more.then_some(cursor), false))
                }
                KeyKind::Stream => {
                    let rows = xrange_rows(next_reply()?)?;
                    let (batch, shown) = fill(&rows, want, STREAM);
                    if let Some(last) = shown.checked_sub(1).and_then(|i| rows.get(i)) {
                        cursor.last_id = Some(last[0].clone());
                    }
                    let more = shown < rows.len() || rows.len() >= want;
                    Ok(page(STREAM, batch, more.then_some(cursor), false))
                }
                KeyKind::Other(name) => {
                    let bytes = match next_reply()? {
                        Value::Int(n) => n.to_string().into_bytes(),
                        Value::Nil => Vec::new(),
                        _ => return Err(unexpected("MEMORY USAGE")),
                    };
                    let (batch, _) = fill(&[vec![name.clone().into_bytes(), bytes]], 1, MODULE);
                    Ok(page(MODULE, batch, None, false))
                }
            }
        }
    }

    fn columns(layout: Layout) -> Vec<ColumnInfo> {
        layout
            .iter()
            .map(|(name, ty, _)| ColumnInfo::new(*name, *ty, None))
            .collect()
    }

    /// The page of a key that does not exist: no columns, no rows, nothing after.
    fn gone() -> KeyValuePage {
        KeyValuePage {
            kind: KeyKind::Other("none".to_owned()),
            ttl: KeyTtl::Gone,
            length: None,
            columns: Vec::new(),
            batch: BatchBuilder::new(0).finish(),
            next: None,
            truncated_value: false,
        }
    }

    /// Pushes rows until `want` are shown or the next would take the batch past
    /// `MAX_BATCH_BYTES`, and returns the batch and how many rows it holds. The first row
    /// always goes in, so one huge row still shows.
    fn fill(rows: &[Row], want: usize, layout: Layout) -> (ResultBatch, usize) {
        let mut builder = BatchBuilder::new(layout.len());
        let mut shown = 0;
        for row in rows.iter().take(want) {
            let size: usize = row.iter().map(Vec::len).sum();
            if shown > 0 && builder.total_bytes() as usize + size > MAX_BATCH_BYTES {
                break;
            }
            let cells: Vec<CellRef<'_>> = row
                .iter()
                .map(|c| match std::str::from_utf8(c) {
                    Ok(t) => CellRef::Text(t),
                    Err(_) => CellRef::Bytes(c),
                })
                .collect();
            let closed = builder.push_typed_row(&cells, |i| layout[i].2);
            debug_assert!(closed.is_none(), "the byte bound keeps one batch");
            shown += 1;
        }
        (builder.finish(), shown)
    }

    /// A capped string cut inside a UTF-8 character drops the partial character, so it still
    /// reads as text.
    fn whole_chars(mut value: Vec<u8>, truncated: bool) -> Vec<u8> {
        if truncated
            && let Err(e) = std::str::from_utf8(&value)
            && e.error_len().is_none()
        {
            value.truncate(e.valid_up_to());
        }
        value
    }

    fn bulk(reply: Value, what: &str) -> Result<Vec<u8>, KeyBrowseError> {
        match reply {
            Value::BulkString(b) => Ok(b),
            Value::SimpleString(s) => Ok(s.into_bytes()),
            _ => Err(unexpected(what)),
        }
    }

    fn array(reply: Value, what: &str) -> Result<Vec<Value>, KeyBrowseError> {
        match reply {
            Value::Array(items) | Value::Set(items) => Ok(items),
            _ => Err(unexpected(what)),
        }
    }

    /// `ZRANGE .. WITHSCORES`: `[member, score]` pairs under RESP3, a flat list under RESP2.
    fn zrange_rows(reply: Value) -> Result<Vec<Row>, KeyBrowseError> {
        let items = array(reply, "ZRANGE")?;
        let flat: Vec<Value> = if items.iter().all(|i| matches!(i, Value::Array(_))) {
            items
                .into_iter()
                .flat_map(|i| array(i, "ZRANGE").unwrap_or_default())
                .collect()
        } else {
            items
        };
        if !flat.len().is_multiple_of(2) {
            return Err(unexpected("ZRANGE"));
        }
        let mut flat = flat.into_iter();
        let mut rows = Vec::new();
        while let (Some(member), Some(score)) = (flat.next(), flat.next()) {
            let score = match score {
                Value::Double(f) => double_text(f).into_bytes(),
                other => bulk(other, "ZRANGE")?,
            };
            rows.push(vec![bulk(member, "ZRANGE")?, score]);
        }
        Ok(rows)
    }

    /// `XRANGE`: each entry is `[id, [field, value, ...]]`.
    fn xrange_rows(reply: Value) -> Result<Vec<Row>, KeyBrowseError> {
        array(reply, "XRANGE")?
            .into_iter()
            .map(|entry| {
                let mut parts = array(entry, "XRANGE")?.into_iter();
                let (Some(id), Some(fields), None) = (parts.next(), parts.next(), parts.next())
                else {
                    return Err(unexpected("XRANGE"));
                };
                let fields = array(fields, "XRANGE")?;
                Ok(vec![bulk(id, "XRANGE")?, fields_json(&fields).into_bytes()])
            })
            .collect()
    }

    /// A stream entry's flat field list as a compact JSON object; a non-UTF-8 string is
    /// {"$bytes": base64}.
    pub(crate) fn fields_json(fields: &[Value]) -> String {
        let object = fields
            .as_chunks::<2>()
            .0
            .iter()
            .map(|[k, v]| Ok((json_key(k)?, json(v)?)))
            .collect::<Result<serde_json::Map<_, _>, crate::redis::shape::ShapeError>>();
        match object {
            Ok(object) => serde_json::Value::Object(object).to_string(),
            // Stream fields are always strings; anything else shows as what went wrong.
            Err(e) => e.to_string(),
        }
    }
}

#[cfg(all(test, feature = "redis"))]
mod tests;
