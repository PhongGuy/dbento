//! Completion for MongoDB shell text: collections, database, collection and cursor methods,
//! query operators and constructor helpers, decided from the text before the caret.

use crate::catalog::CatalogQuery;
use crate::completion::{Candidate, CandidateKind, Completion, MAX_ITEMS, kind_detail, starts};
use crate::schema_tree::CatalogTree;

/// Methods offered on `db` and on `db.getSiblingDB(..)`.
pub const DB_METHODS: &[&str] = &[
    "getCollection",
    "getSiblingDB",
    "getCollectionNames",
    "createCollection",
    "dropDatabase",
    "stats",
    "runCommand",
];

/// Methods offered on a collection.
pub const COLLECTION_METHODS: &[&str] = &[
    "find",
    "findOne",
    "aggregate",
    "countDocuments",
    "estimatedDocumentCount",
    "distinct",
    "getIndexes",
    "insertOne",
    "insertMany",
    "updateOne",
    "updateMany",
    "replaceOne",
    "deleteOne",
    "deleteMany",
    "findOneAndUpdate",
    "findOneAndReplace",
    "findOneAndDelete",
    "createIndex",
    "dropIndex",
    "drop",
];

/// Methods offered on the cursor a `find(..)` returns.
pub const CURSOR_METHODS: &[&str] = &[
    "sort",
    "limit",
    "skip",
    "projection",
    "hint",
    "collation",
    "comment",
    "batchSize",
];

/// Value constructors offered inside call arguments.
pub const HELPERS: &[&str] = &[
    "ObjectId",
    "ISODate",
    "Date",
    "NumberInt",
    "Int32",
    "NumberLong",
    "Long",
    "Double",
    "NumberDecimal",
    "Decimal128",
    "UUID",
    "BinData",
    "Timestamp",
    "MinKey",
    "MaxKey",
    "RegExp",
];

/// Query, update and aggregation operators offered on a `$` word.
pub const OPERATORS: &[&str] = &[
    "$eq",
    "$ne",
    "$gt",
    "$gte",
    "$lt",
    "$lte",
    "$in",
    "$nin",
    "$and",
    "$or",
    "$nor",
    "$not",
    "$exists",
    "$type",
    "$regex",
    "$expr",
    "$elemMatch",
    "$size",
    "$all",
    "$mod",
    "$text",
    "$where",
    "$set",
    "$unset",
    "$inc",
    "$mul",
    "$rename",
    "$min",
    "$max",
    "$push",
    "$pull",
    "$addToSet",
    "$pop",
    "$currentDate",
    "$setOnInsert",
    "$match",
    "$project",
    "$group",
    "$sort",
    "$limit",
    "$skip",
    "$unwind",
    "$lookup",
    "$addFields",
    "$count",
    "$facet",
    "$unionWith",
    "$out",
    "$merge",
    "$sample",
    "$replaceRoot",
    "$sortByCount",
    "$sum",
    "$avg",
    "$first",
    "$last",
];

#[derive(Debug, Clone, Copy, PartialEq)]
enum Kind {
    Ident,
    Str,
    Comment,
    Regex,
    Punct(u8),
}

#[derive(Debug, Clone)]
struct Tok {
    kind: Kind,
    start: usize,
    end: usize,
    /// The token reaches the caret without its closing delimiter.
    open: bool,
}

/// What the expression before a `.` evaluates to.
enum Recv {
    /// `db`, or `db.getSiblingDB("<name>")` with its name.
    Db(Option<String>),
    Collection,
    Cursor,
    Other,
}

fn is_word(b: u8) -> bool {
    b.is_ascii_alphanumeric() || b == b'_' || b == b'$'
}

/// Splits the text before the caret into tokens. A `/` opens a regex literal only where a value
/// starts, so division never hides the rest of the line.
fn scan(s: &str) -> Vec<Tok> {
    let b = s.as_bytes();
    let mut toks: Vec<Tok> = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        let start = i;
        let value_starts = toks
            .iter()
            .rev()
            .find(|t| t.kind != Kind::Comment)
            .is_none_or(|t| matches!(t.kind, Kind::Punct(b'(' | b'[' | b'{' | b',' | b':')));
        let (kind, end, open) = if c.is_ascii_whitespace() {
            i += 1;
            continue;
        } else if s[i..].starts_with("//") {
            let end = s[i..].find('\n').map_or(b.len(), |n| i + n);
            (Kind::Comment, end, end == b.len())
        } else if s[i..].starts_with("/*") {
            match s[i + 2..].find("*/") {
                Some(n) => (Kind::Comment, i + 2 + n + 2, false),
                None => (Kind::Comment, b.len(), true),
            }
        } else if matches!(c, b'\'' | b'"' | b'`') {
            let (end, open) = delimited(b, i, c);
            (Kind::Str, end, open)
        } else if c == b'/' && value_starts {
            let (mut end, open) = delimited(b, i, b'/');
            while !open && end < b.len() && b[end].is_ascii_alphabetic() {
                end += 1;
            }
            (Kind::Regex, end, open)
        } else if is_word(c) {
            let end = s[i..]
                .bytes()
                .position(|x| !is_word(x))
                .map_or(b.len(), |n| i + n);
            (Kind::Ident, end, false)
        } else {
            let len = s[i..].chars().next().map_or(1, char::len_utf8);
            (
                Kind::Punct(if c.is_ascii() { c } else { 0 }),
                i + len,
                false,
            )
        };
        toks.push(Tok {
            kind,
            start,
            end,
            open,
        });
        i = end;
    }
    toks
}

/// The end of the run at `i` closed by `q` with backslash escapes, and whether it is still open.
fn delimited(b: &[u8], i: usize, q: u8) -> (usize, bool) {
    let mut j = i + 1;
    while j < b.len() {
        match b[j] {
            b'\\' => j += 2,
            x if x == q => return (j + 1, false),
            _ => j += 1,
        }
    }
    (b.len(), true)
}

/// `complete_in` for the MongoDB shell: `stmt` starts at buffer byte `start`, the caret is at
/// buffer byte `offset`; collections come from `default_schema` unless `getSiblingDB` names one.
pub(crate) fn complete_mongo(
    stmt: &str,
    start: usize,
    offset: usize,
    tree: &CatalogTree,
    conn: &str,
    default_schema: Option<&str>,
) -> Completion {
    let mut out = Completion {
        replace: offset..offset,
        items: Vec::new(),
        wants: Vec::new(),
    };
    let Some(off) = offset.checked_sub(start) else {
        return out;
    };
    let Some(before) = stmt.get(..off) else {
        return out;
    };
    let mut toks = scan(before);
    if toks.last().is_some_and(|t| t.open) {
        return out;
    }
    toks.retain(|t| t.kind != Kind::Comment);
    // The word is an identifier touching the caret; what precedes it decides the site.
    let word_start = match toks.last() {
        Some(t) if t.kind == Kind::Ident && t.end == off => t.start,
        _ => off,
    };
    if word_start < off {
        toks.pop();
    }
    let word = &before[word_start..];
    if word.starts_with(|c: char| c.is_ascii_digit()) {
        return out;
    }
    out.replace = start + word_start..offset;
    let src = before;
    let items = &mut out.items;
    if word.starts_with('$') {
        items.extend(keywords(OPERATORS, word, "operator"));
    } else if toks.last().is_some_and(|t| t.kind == Kind::Punct(b'.')) {
        let dot = toks.len() - 1;
        match receiver(&toks, src, dot.checked_sub(1)) {
            Recv::Db(name) => {
                if let Some(db) = name.as_deref().or(default_schema) {
                    match tree.relations(conn, db) {
                        Some(list) => {
                            let mut cols: Vec<Candidate> = list
                                .items()
                                .iter()
                                .filter(|r| starts(r.name(), word))
                                .map(|r| collection(r.name(), kind_detail(r.kind())))
                                .collect();
                            cols.sort_by_cached_key(|c| c.label.to_lowercase());
                            items.extend(cols);
                        }
                        None => out.wants.push(CatalogQuery::Relations {
                            schema: db.to_owned(),
                        }),
                    }
                }
                items.extend(keywords(DB_METHODS, word, "method"));
            }
            Recv::Collection => items.extend(keywords(COLLECTION_METHODS, word, "method")),
            Recv::Cursor => items.extend(keywords(CURSOR_METHODS, word, "method")),
            Recv::Other => {}
        }
    } else if word.is_empty() {
        // A bare caret with no word and no dot offers nothing, as SQL's completer does.
    } else if toks.is_empty() {
        items.extend(keywords(&["db", "use"], word, "keyword"));
    } else if toks.len() == 1 && &src[toks[0].start..toks[0].end] == "use" {
        match tree.schemas(conn) {
            Some(list) => {
                items.extend(
                    list.iter()
                        .filter(|s| starts(s.name(), word))
                        .map(|s| Candidate {
                            label: s.name().to_owned(),
                            insert: s.name().to_owned(),
                            kind: CandidateKind::Schema,
                            detail: "schema".into(),
                        }),
                )
            }
            None => out.wants.push(CatalogQuery::Schemas),
        }
    } else if open_brackets(&toks) > 0 {
        items.extend(keywords(HELPERS, word, "helper"));
    }
    out.items.truncate(MAX_ITEMS);
    out
}

/// How many brackets are open after `toks`.
fn open_brackets(toks: &[Tok]) -> i32 {
    toks.iter().fold(0, |depth, t| match t.kind {
        Kind::Punct(b'(' | b'[' | b'{') => depth + 1,
        Kind::Punct(b')' | b']' | b'}') => depth - 1,
        _ => depth,
    })
}

/// What the expression ending at token `last` evaluates to, walking the chain back to `db`.
fn receiver(toks: &[Tok], src: &str, last: Option<usize>) -> Recv {
    let Some(last) = last else {
        return Recv::Other;
    };
    let t = &toks[last];
    let text = |t: &Tok| &src[t.start..t.end];
    let dotted = |i: usize| i > 0 && toks[i - 1].kind == Kind::Punct(b'.');
    match t.kind {
        Kind::Ident if text(t) == "db" && !dotted(last) => Recv::Db(None),
        Kind::Ident if dotted(last) => match receiver(toks, src, last.checked_sub(2)) {
            Recv::Db(_) => Recv::Collection,
            _ => Recv::Other,
        },
        Kind::Punct(close @ (b')' | b']')) => {
            let Some(open) = matching(toks, last, close) else {
                return Recv::Other;
            };
            let arg = toks
                .get(open + 1)
                .filter(|a| a.kind == Kind::Str)
                .map(|a| unquote(text(a)));
            if close == b']' {
                return match receiver(toks, src, open.checked_sub(1)) {
                    Recv::Db(_) => Recv::Collection,
                    _ => Recv::Other,
                };
            }
            let Some(m) = open.checked_sub(1).filter(|&m| dotted(m)) else {
                return Recv::Other;
            };
            let method = text(&toks[m]);
            match (receiver(toks, src, m.checked_sub(2)), method) {
                (Recv::Db(_), "getSiblingDB") => Recv::Db(arg),
                (Recv::Db(_), "getCollection") => Recv::Collection,
                (Recv::Collection, "find") => Recv::Cursor,
                (Recv::Cursor, m) if CURSOR_METHODS.contains(&m) => Recv::Cursor,
                _ => Recv::Other,
            }
        }
        _ => Recv::Other,
    }
}

/// The index of the bracket that `close` at `at` closes.
fn matching(toks: &[Tok], at: usize, close: u8) -> Option<usize> {
    let open = if close == b')' { b'(' } else { b'[' };
    let mut depth = 0;
    for i in (0..=at).rev() {
        match toks[i].kind {
            Kind::Punct(c) if c == close => depth += 1,
            Kind::Punct(c) if c == open => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// A closed string literal's contents with its backslash escapes undone.
fn unquote(lit: &str) -> String {
    let mut out = String::new();
    let mut chars = lit[1..lit.len() - 1].chars();
    while let Some(c) = chars.next() {
        out.push(if c == '\\' {
            chars.next().unwrap_or(c)
        } else {
            c
        });
    }
    out
}

/// A collection candidate: an identifier inserts bare, any other name through `getCollection`.
fn collection(name: &str, detail: &str) -> Candidate {
    let ident = name.starts_with(|c: char| c.is_ascii_alphabetic() || c == '_' || c == '$')
        && name.bytes().all(is_word);
    let insert = if ident {
        name.to_owned()
    } else {
        let quoted = serde_json::to_string(name).unwrap_or_default();
        format!("getCollection({quoted})")
    };
    Candidate {
        label: name.to_owned(),
        insert,
        kind: CandidateKind::Relation,
        detail: detail.to_owned(),
    }
}

fn keywords<'a>(
    list: &'a [&str],
    prefix: &'a str,
    detail: &'a str,
) -> impl Iterator<Item = Candidate> + 'a {
    list.iter()
        .filter(|k| starts(k, prefix))
        .map(move |k| Candidate {
            label: (*k).to_owned(),
            insert: (*k).to_owned(),
            kind: CandidateKind::Keyword,
            detail: detail.to_owned(),
        })
}
