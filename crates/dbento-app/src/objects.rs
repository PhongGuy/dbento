//! The S3 browser's own copies of core's object types, and the texts every S3 view shows:
//! row labels, `s3://` URLs, sizes and error messages, each with keys escaped for display.

use dbento_core::{
    ExecuteError, IntrospectError, ObjectBrowseError, ObjectCursor, ObjectMeta, display_key,
};

/// How many entries one tree page of a bucket level asks for.
pub const OBJECT_PAGE: u32 = 500;

/// One object as a listing reports it; a plain copy of core's `ObjectMeta`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectItem {
    /// The raw key, not escaped.
    pub key: String,
    /// Size in bytes.
    pub size: u64,
    /// When the object was last written, as RFC 3339 UTC.
    pub last_modified: Option<String>,
    /// The entity tag without its quotes.
    pub etag: Option<String>,
    /// The storage class, such as `STANDARD`.
    pub storage_class: Option<String>,
    /// The MIME type, when a HEAD or GET reported it.
    pub content_type: Option<String>,
}

impl From<ObjectMeta> for ObjectItem {
    fn from(m: ObjectMeta) -> Self {
        ObjectItem {
            key: m.key,
            size: m.size,
            last_modified: m.last_modified,
            etag: m.etag,
            storage_class: m.storage_class,
            content_type: m.content_type,
        }
    }
}

/// One row of a bucket level: a common prefix or an object, each by its raw key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObjectEntry {
    /// A common prefix, ending in the delimiter.
    Prefix(String),
    /// An object.
    Object(ObjectItem),
}

impl ObjectEntry {
    fn key(&self) -> &str {
        match self {
            ObjectEntry::Prefix(p) => p,
            ObjectEntry::Object(o) => &o.key,
        }
    }
}

/// One page of a level, and where the next page resumes when there is one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObjectBatch {
    /// The page's entries in key order.
    pub entries: Vec<ObjectEntry>,
    /// The cursor of the next page; `None` on the last.
    pub next: Option<ObjectCursor>,
}

/// A page's prefixes and objects as one list in key order, a prefix before an object of the
/// same key (the folder `a/` before its marker object `a/`).
pub fn merge_page(prefixes: Vec<String>, objects: Vec<ObjectItem>) -> Vec<ObjectEntry> {
    let mut entries: Vec<ObjectEntry> = prefixes
        .into_iter()
        .map(ObjectEntry::Prefix)
        .chain(objects.into_iter().map(ObjectEntry::Object))
        .collect();
    // The sort is stable and prefixes come first, so a tie keeps the prefix ahead.
    entries.sort_by(|a, b| a.key().cmp(b.key()));
    entries
}

/// The label of the row for `key` under the level `parent`: the key past the parent, one
/// trailing `/` dropped, escaped; "(empty name)" when nothing is left.
pub fn segment_label(parent: &str, key: &str) -> String {
    let rest = key.strip_prefix(parent).unwrap_or(key);
    let rest = rest.strip_suffix('/').unwrap_or(rest);
    if rest.is_empty() {
        "(empty name)".to_owned()
    } else {
        display_key(rest)
    }
}

/// `s3://bucket/key` with the bucket and key escaped for display.
pub fn s3_url(bucket: &str, key: &str) -> String {
    format!("s3://{}/{}", display_key(bucket), display_key(key))
}

/// A byte count in words: whole bytes below 1 KiB, else one decimal in the largest binary unit.
pub fn size_words(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["KiB", "MiB", "GiB", "TiB", "PiB"];
    match bytes {
        1 => "1 byte".to_owned(),
        0..1024 => format!("{bytes} bytes"),
        _ => {
            let mut value = bytes as f64 / 1024.0;
            let mut unit = 0;
            while value >= 1024.0 && unit + 1 < UNITS.len() {
                value /= 1024.0;
                unit += 1;
            }
            format!("{value:.1} {}", UNITS.get(unit).unwrap_or(&"PiB"))
        }
    }
}

/// An object-browser error as the app shows it: keys and every store text escaped, a hint on
/// its own line.
pub fn object_error_text(e: &ObjectBrowseError) -> String {
    match e {
        ObjectBrowseError::NotFound { key } => format!("not found: {}", display_key(key)),
        ObjectBrowseError::AccessDenied { message } => {
            format!("access denied: {}", display_key(message))
        }
        ObjectBrowseError::Database {
            code,
            message,
            hint,
        } => with_hint(
            format!("{} ({})", display_key(message), display_key(code)),
            hint.as_deref().map(display_key).as_deref(),
        ),
        // The hint may name a region the server sent.
        ObjectBrowseError::WrongRegion { hint } => format!("wrong region: {}", display_key(hint)),
        ObjectBrowseError::Unsupported { .. }
        | ObjectBrowseError::Timeout
        | ObjectBrowseError::SessionEnded { .. }
        | _ => e.to_string(),
    }
}

/// A catalog error as the app shows it, with the database's hint on its own line when it
/// gave one.
pub fn introspect_message(e: &IntrospectError) -> String {
    let hint = match e {
        IntrospectError::Execute(ExecuteError::Database { hint, .. }) => hint.as_deref(),
        IntrospectError::Execute(_)
        | IntrospectError::Unexpected { .. }
        | IntrospectError::Missing { .. }
        | IntrospectError::Unsupported { .. }
        | _ => None,
    };
    with_hint(e.to_string(), hint)
}

/// A bucket listing error as [`introspect_message`] gives it, with the store's message, code
/// and hint escaped, since on S3 they are text from the server.
pub fn object_listing_message(e: &IntrospectError) -> String {
    match e {
        IntrospectError::Execute(ExecuteError::Database {
            code,
            message,
            hint,
            ..
        }) => with_hint(
            format!("{} ({})", display_key(message), display_key(code)),
            hint.as_deref().map(display_key).as_deref(),
        ),
        IntrospectError::Execute(_)
        | IntrospectError::Unexpected { .. }
        | IntrospectError::Missing { .. }
        | IntrospectError::Unsupported { .. }
        | _ => display_key(&e.to_string()),
    }
}

fn with_hint(text: String, hint: Option<&str>) -> String {
    match hint {
        Some(h) => format!("{text}\nhint: {h}"),
        None => text,
    }
}
