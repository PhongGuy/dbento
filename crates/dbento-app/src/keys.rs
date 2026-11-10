//! Redis key browsing on the app side: mirror types of core's key pages, the key filter, how
//! a key name, kind and TTL read, and the `:` grouping the key tree draws.

use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

use dbento_core::{KeyCursor, KeyKind, KeyTtl};

/// How many keys one page of the key tree asks for.
pub const KEY_PAGE: u32 = 500;

/// One listed key.
#[derive(Debug, Clone, PartialEq)]
pub struct KeyItem {
    /// The key's name, which may not be UTF-8.
    pub name: Vec<u8>,
    /// What `TYPE` named it.
    pub kind: KeyKind,
    /// What `PTTL` answered.
    pub ttl: KeyTtl,
}

/// One page of keys and where the walk goes on; `next` is None once the scan finished.
#[derive(Debug, Clone, PartialEq)]
pub struct KeyBatch {
    /// The keys of this page.
    pub items: Vec<KeyItem>,
    /// The cursor of the next page.
    pub next: Option<KeyToken>,
}

/// A shared scan cursor that compares and hashes by identity, so a query carrying one keeps
/// `Eq` and `Hash` whatever the cursor itself derives, and a reply matches only its own page.
#[derive(Debug, Clone)]
pub struct KeyToken(Arc<KeyCursor>);

impl KeyToken {
    /// Wraps `cursor`.
    pub fn new(cursor: KeyCursor) -> Self {
        KeyToken(Arc::new(cursor))
    }

    /// The cursor the next scan resumes from.
    pub fn cursor(&self) -> &KeyCursor {
        &self.0
    }
}

impl PartialEq for KeyToken {
    fn eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
}

impl Eq for KeyToken {}

impl Hash for KeyToken {
    fn hash<H: Hasher>(&self, state: &mut H) {
        Arc::as_ptr(&self.0).hash(state);
    }
}

/// Which keys a listing scans: a `MATCH` pattern and a `TYPE` name, each optional.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash)]
pub struct KeyFilter {
    /// The glob pattern; None matches every key.
    pub pattern: Option<String>,
    /// The `TYPE` name, such as `hash` or `zset`.
    pub kind: Option<String>,
}

/// Reads `<pattern> [type:<kind>]`; empty text or a lone `*` means every key. Errors name
/// what is wrong for the filter prompt.
pub fn parse_key_filter(text: &str) -> Result<KeyFilter, String> {
    let mut filter = KeyFilter::default();
    let mut pattern_seen = false;
    for word in text.split_whitespace() {
        if let Some(kind) = word.strip_prefix("type:") {
            if kind.is_empty() {
                return Err("type: needs a type name, such as type:hash".into());
            }
            if filter.kind.replace(kind.to_owned()).is_some() {
                return Err("give one type: at most".into());
            }
        } else if std::mem::replace(&mut pattern_seen, true) {
            return Err("give one pattern, such as user:*".into());
        } else if word != "*" {
            filter.pattern = Some(word.to_owned());
        }
    }
    Ok(filter)
}

/// A key name as text: itself when it is UTF-8 with no control characters, so Copy name gives
/// a plain key verbatim; otherwise every byte outside printable ASCII as `\xHH`, with `\`
/// doubled. A plain name spelling `\xHH` can read like an escaped one; rows keep the raw bytes.
pub fn display_key(name: &[u8]) -> String {
    if let Ok(text) = std::str::from_utf8(name)
        && !text.chars().any(char::is_control)
    {
        return text.to_owned();
    }
    let mut out = String::with_capacity(name.len());
    for &b in name {
        match b {
            b'\\' => out.push_str("\\\\"),
            0x20..=0x7e => out.push(char::from(b)),
            _ => out.push_str(&format!("\\x{b:02x}")),
        }
    }
    out
}

/// A TTL in its largest whole unit (`90s` reads `1m`), `<1s` under a second, empty for a key
/// that never expires and `gone` for one that no longer exists.
pub fn ttl_text(ttl: &KeyTtl) -> String {
    let left = match ttl {
        KeyTtl::Expires(d) => d.as_secs(),
        KeyTtl::Gone => return "gone".into(),
        KeyTtl::Persistent | _ => return String::new(),
    };
    match left {
        0 => "<1s".into(),
        s if s < 60 => format!("{s}s"),
        s if s < 3_600 => format!("{}m", s / 60),
        s if s < 86_400 => format!("{}h", s / 3_600),
        s => format!("{}d", s / 86_400),
    }
}

/// The name `TYPE` gives `kind`; a module type gives its own.
pub fn type_name(kind: &KeyKind) -> String {
    match kind {
        KeyKind::String => "string",
        KeyKind::Hash => "hash",
        KeyKind::List => "list",
        KeyKind::Set => "set",
        KeyKind::SortedSet => "zset",
        KeyKind::Stream => "stream",
        KeyKind::Other(name) => name,
        _ => "unknown",
    }
    .to_owned()
}

/// The kind a `TYPE` name stands for.
pub fn kind_of(name: &str) -> KeyKind {
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

/// One level of the key tree below a prefix: its folders with their key counts, then its
/// leaves as indexes into the names given, each sorted by bytes.
#[derive(Debug, Clone, PartialEq)]
pub struct KeyLevel {
    /// Each folder's full prefix, ending in `:`, and how many keys sit below it.
    pub folders: Vec<(Vec<u8>, usize)>,
    /// The keys directly at this level.
    pub leaves: Vec<usize>,
}

/// Groups the `names` under `prefix` by their next `:`: a name with one more `:` after the
/// prefix counts toward the folder up to it, any other is a leaf of this level.
pub fn key_levels(names: &[&[u8]], prefix: &[u8]) -> KeyLevel {
    let mut folders: BTreeMap<&[u8], usize> = BTreeMap::new();
    let mut leaves = Vec::new();
    for (i, name) in names.iter().enumerate() {
        let Some(rest) = name.strip_prefix(prefix) else {
            continue;
        };
        match rest.iter().position(|&b| b == b':') {
            Some(at) => *folders.entry(&name[..prefix.len() + at + 1]).or_default() += 1,
            None => leaves.push(i),
        }
    }
    leaves.sort_by(|&a, &b| names[a].cmp(names[b]));
    let folders = folders.into_iter().map(|(f, n)| (f.to_vec(), n)).collect();
    KeyLevel { folders, leaves }
}
