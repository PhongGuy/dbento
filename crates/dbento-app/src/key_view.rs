//! A Redis key's value tab: where its paged read stands and the header that words the key's
//! kind, length and time to live above the grid.

use dbento_core::{ColumnInfo, KeyKind, KeyRead, KeyTtl, KeyValuePage, ResultBatch, ValueCursor};

use crate::keys::{display_key, ttl_text, type_name};

/// How many rows one value page asks for.
pub const VALUE_PAGE: u32 = 500;

/// The header over a value: kind, length in the kind's own unit, TTL and, for a string past
/// the page cap, that only its start is shown. A key that is gone says only that.
pub fn header_text(
    kind: &KeyKind,
    ttl: &KeyTtl,
    length: Option<u64>,
    truncated_value: bool,
) -> String {
    if *ttl == KeyTtl::Gone {
        return "the key no longer exists".to_owned();
    }
    let mut parts = vec![type_name(kind)];
    if let Some(n) = length {
        let (one, many) = match kind {
            KeyKind::String => ("byte", "bytes"),
            KeyKind::Hash => ("field", "fields"),
            KeyKind::List => ("element", "elements"),
            KeyKind::Stream => ("entry", "entries"),
            KeyKind::Set | KeyKind::SortedSet | KeyKind::Other(_) | _ => ("member", "members"),
        };
        parts.push(format!("{n} {}", if n == 1 { one } else { many }));
    }
    parts.push(match ttl_text(ttl) {
        t if t.is_empty() => "no TTL".to_owned(),
        t => format!("expires in {t}"),
    });
    if truncated_value {
        parts.push("first 1 MiB shown".to_owned());
    }
    parts.join(" \u{B7} ")
}

/// One key's value read page by page: the first read starts the value, each later one resumes
/// where the last page ended, and only one read is in flight at a time.
#[derive(Debug, Clone)]
pub struct KeyView {
    db: u32,
    key: Vec<u8>,
    /// The kind the last page read, so a read that restarted on a new kind can be told apart.
    kind: Option<KeyKind>,
    header: Option<String>,
    next: Option<ValueCursor>,
    reading: bool,
    error: Option<String>,
}

impl KeyView {
    /// A view of `key` in DB `db` with nothing read yet.
    pub fn new(db: u32, key: Vec<u8>) -> Self {
        KeyView {
            db,
            key,
            kind: None,
            header: None,
            next: None,
            reading: false,
            error: None,
        }
    }

    /// The tab's title: the key as text and its DB.
    pub fn title(&self) -> String {
        format!("{} \u{B7} db {}", display_key(&self.key), self.db)
    }

    /// The next page to ask for: the first until a page has landed, then from where it ended;
    /// None while a read is in flight or once the value is read to its end.
    pub fn read(&self) -> Option<KeyRead> {
        if self.reading {
            return None;
        }
        let from = match (&self.header, &self.next) {
            (None, _) => ValueCursor::start(),
            (Some(_), Some(next)) => next.clone(),
            (Some(_), None) => return None,
        };
        let mut read = KeyRead::new(self.db, self.key.clone());
        read.from = from;
        read.want = VALUE_PAGE;
        Some(read)
    }

    /// Marks a read as in flight.
    pub fn begin(&mut self) {
        self.reading = true;
        self.error = None;
    }

    /// Takes a landed page's header and resume point and hands back its columns and rows.
    pub fn apply(&mut self, page: KeyValuePage) -> (Vec<ColumnInfo>, ResultBatch) {
        self.header = Some(header_text(
            &page.kind,
            &page.ttl,
            page.length,
            page.truncated_value,
        ));
        self.kind = Some(page.kind);
        self.next = page.next;
        self.reading = false;
        (page.columns, page.batch)
    }

    /// Ends the read in flight with `message`; the same page can be asked for again.
    pub fn fail(&mut self, message: String) {
        self.reading = false;
        self.error = Some(message);
    }

    /// The header, once a page has landed.
    pub fn header(&self) -> Option<&str> {
        self.header.as_deref()
    }

    /// The kind the last page read, once one has landed.
    pub fn kind(&self) -> Option<&KeyKind> {
        self.kind.as_ref()
    }

    /// Why the last read failed, until the next one begins.
    pub fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Whether pages remain after the ones landed.
    pub fn has_more(&self) -> bool {
        self.header.is_some() && self.next.is_some()
    }
}
