//! The SQL completion model: what to offer at a caret, from the catalog levels already cached in
//! a [`CatalogTree`]. Pure data with no GPUI types; it never queries, it names what it lacks.

use std::ops::Range;

use dbento_core::{Engine, RelationKind, RelationRef, quote_ident, statement_ranges};

use crate::catalog::CatalogQuery;
use crate::schema_tree::CatalogTree;

/// What a candidate names.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CandidateKind {
    /// A column of a FROM or JOIN relation.
    Column,
    /// A table, view or other relation.
    Relation,
    /// A schema.
    Schema,
    /// An SQL keyword.
    Keyword,
}

/// One completion item.
#[derive(Debug, Clone, PartialEq)]
pub struct Candidate {
    /// The name, or the keyword as inserted.
    pub label: String,
    /// The text to insert: the quoted name, or the keyword.
    pub insert: String,
    /// What the candidate names.
    pub kind: CandidateKind,
    /// The column type, the relation kind, "schema" or "keyword".
    pub detail: String,
}

/// The answer at one caret.
#[derive(Debug, Clone, PartialEq)]
pub struct Completion {
    /// The identifier prefix before the caret, which an accepted item replaces.
    pub replace: Range<usize>,
    /// The candidates, columns first, then relations, schemas and keywords.
    pub items: Vec<Candidate>,
    /// Catalog levels this answer needed but found unloaded.
    pub wants: Vec<CatalogQuery>,
}

/// The most candidates one answer holds.
pub const MAX_ITEMS: usize = 100;

/// The keywords offered, uppercase.
pub const KEYWORDS: &[&str] = &[
    "SELECT",
    "FROM",
    "WHERE",
    "AND",
    "OR",
    "NOT",
    "NULL",
    "IS",
    "IN",
    "LIKE",
    "ILIKE",
    "BETWEEN",
    "EXISTS",
    "JOIN",
    "LEFT",
    "RIGHT",
    "INNER",
    "OUTER",
    "FULL",
    "CROSS",
    "ON",
    "USING",
    "GROUP",
    "BY",
    "ORDER",
    "HAVING",
    "LIMIT",
    "OFFSET",
    "DISTINCT",
    "AS",
    "CASE",
    "WHEN",
    "THEN",
    "ELSE",
    "END",
    "UNION",
    "ALL",
    "INTERSECT",
    "EXCEPT",
    "INSERT",
    "INTO",
    "VALUES",
    "UPDATE",
    "SET",
    "DELETE",
    "RETURNING",
    "CREATE",
    "TABLE",
    "VIEW",
    "INDEX",
    "DROP",
    "ALTER",
    "ADD",
    "COLUMN",
    "PRIMARY",
    "KEY",
    "FOREIGN",
    "REFERENCES",
    "DEFAULT",
    "WITH",
    "RECURSIVE",
    "ASC",
    "DESC",
    "BEGIN",
    "COMMIT",
    "ROLLBACK",
    "EXPLAIN",
    "ANALYZE",
    "TRUE",
    "FALSE",
    "CAST",
];

/// The SurrealQL keywords offered, uppercase.
pub const SURREAL_KEYWORDS: &[&str] = &[
    "SELECT", "FROM", "WHERE", "AND", "OR", "NOT", "IS", "NULL", "NONE", "TRUE", "FALSE", "ONLY",
    "VALUE", "AS", "GROUP", "ALL", "ORDER", "BY", "ASC", "DESC", "LIMIT", "START", "FETCH",
    "SPLIT", "OMIT", "CREATE", "UPDATE", "UPSERT", "DELETE", "INSERT", "INTO", "RELATE", "CONTENT",
    "MERGE", "SET", "UNSET", "RETURN", "LET", "IF", "ELSE", "THEN", "END", "FOR", "BEGIN",
    "COMMIT", "CANCEL", "INFO", "DEFINE", "REMOVE", "TABLE", "FIELD", "INDEX", "TYPE", "USE", "NS",
    "DB", "CONTAINS", "INSIDE",
];

/// The SurrealQL functions offered, as the server spells them (each one runs on v3.3.5).
pub const SURREAL_FUNCTIONS: &[&str] = &[
    "count",
    "array::distinct",
    "array::flatten",
    "array::len",
    "array::sort",
    "crypto::argon2::compare",
    "crypto::argon2::generate",
    "crypto::sha256",
    "duration::secs",
    "math::ceil",
    "math::floor",
    "math::max",
    "math::mean",
    "math::min",
    "math::round",
    "math::sum",
    "object::entries",
    "object::keys",
    "object::values",
    "rand::ulid",
    "rand::uuid",
    "rand::uuid::v7",
    "record::id",
    "record::tb",
    "session::db",
    "session::ns",
    "string::concat",
    "string::contains",
    "string::ends_with",
    "string::join",
    "string::len",
    "string::lowercase",
    "string::replace",
    "string::split",
    "string::starts_with",
    "string::trim",
    "string::uppercase",
    "time::floor",
    "time::format",
    "time::now",
    "type::float",
    "type::int",
    "type::record",
    "type::string",
    "type::table",
];

/// Keywords after which only relations and schemas make sense.
const RELATION_KEYWORDS: [&str; 5] = ["FROM", "JOIN", "INTO", "UPDATE", "TABLE"];

/// Completes at byte `offset` of `text` against `conn`'s cached catalog in `tree`; unqualified
/// relations come from `default_schema`, and none complete when it is None.
pub fn complete(
    engine: Engine,
    text: &str,
    offset: usize,
    tree: &CatalogTree,
    conn: &str,
    default_schema: Option<&str>,
) -> Completion {
    if offset > text.len() || !text.is_char_boundary(offset) {
        return nothing(offset);
    }
    let base = statement_base(&statement_ranges(engine, text), offset, text.len());
    complete_in(
        engine,
        &text[base.clone()],
        base.start,
        offset,
        tree,
        conn,
        default_schema,
    )
}

/// The span completion works in at `offset` of a `len`-byte text split into `ranges`: the
/// statement holding the caret, or for a caret between statements only the gap, which holds
/// whitespace or comments and so completes nothing.
pub fn statement_base(ranges: &[Range<usize>], offset: usize, len: usize) -> Range<usize> {
    ranges
        .iter()
        .find(|r| r.start < offset && offset <= r.end)
        .cloned()
        .unwrap_or_else(|| {
            let start = ranges
                .iter()
                .map(|r| r.end)
                .filter(|&e| e <= offset)
                .max()
                .unwrap_or(0);
            let end = ranges
                .iter()
                .map(|r| r.start)
                .filter(|&s| s >= offset)
                .min()
                .unwrap_or(len);
            start..end
        })
}

fn nothing(offset: usize) -> Completion {
    Completion {
        replace: offset..offset,
        items: Vec::new(),
        wants: Vec::new(),
    }
}

/// [`complete`] given the statement text `stmt` that starts at byte `start` of the buffer,
/// as [`statement_base`] found it; `offset` and the answer count in buffer bytes.
pub fn complete_in(
    engine: Engine,
    stmt: &str,
    start: usize,
    offset: usize,
    tree: &CatalogTree,
    conn: &str,
    default_schema: Option<&str>,
) -> Completion {
    let none = nothing(offset);
    let Some(off) = offset.checked_sub(start) else {
        return none;
    };
    if off > stmt.len() || !stmt.is_char_boundary(off) {
        return none;
    }
    let toks = lex(engine, stmt);
    let Some(site) = site(&toks, stmt, off) else {
        return none;
    };
    let mut ctx = Ctx {
        engine,
        tree,
        conn,
        default_schema,
        wants: Vec::new(),
    };
    let from = from_items(&toks, stmt, off);
    let prefix = &stmt[site.word.clone()];
    let mut out = Vec::new();
    match site.qualifier {
        Some(q) => {
            let mut columns_found = false;
            if !site.relation_context {
                // An alias names its item ahead of any table that shares the spelling.
                let by_alias = |f: &&FromItem| {
                    f.alias
                        .as_deref()
                        .is_some_and(|a| pick(std::iter::once(a), &q).is_some())
                };
                let by_name = |f: &&FromItem| pick(std::iter::once(f.name.as_str()), &q).is_some();
                let mut named: Vec<_> = from.iter().filter(by_alias).collect();
                if named.is_empty() {
                    named = from.iter().filter(by_name).collect();
                }
                if !named.is_empty() {
                    columns_found = true;
                    out.extend(ctx.columns(named, prefix));
                }
            }
            if !columns_found {
                out.extend(ctx.schema_relations(Some(&q), prefix));
            }
        }
        None if site.relation_context => {
            out.extend(ctx.schema_relations(None, prefix));
            out.extend(ctx.schemas(prefix));
        }
        None => {
            out.extend(ctx.columns(&from, prefix));
            out.extend(ctx.schema_relations(None, prefix));
            out.extend(ctx.schemas(prefix));
            out.extend(keywords(engine, prefix));
        }
    }
    out.truncate(MAX_ITEMS);
    Completion {
        replace: start + site.word.start..offset,
        items: out,
        wants: ctx.wants,
    }
}

/// Reads the cache for one answer and records every level it found missing.
struct Ctx<'a> {
    engine: Engine,
    tree: &'a CatalogTree,
    conn: &'a str,
    default_schema: Option<&'a str>,
    wants: Vec<CatalogQuery>,
}

impl Ctx<'_> {
    fn want(&mut self, q: CatalogQuery) {
        if !self.wants.contains(&q) {
            self.wants.push(q);
        }
    }

    /// The loaded schema `name` resolves to, or `name` itself while schemas are unloaded.
    fn schema(&mut self, name: &str) -> Option<String> {
        match self.tree.schemas(self.conn) {
            Some(list) => pick(list.iter().map(|s| s.name()), name).map(str::to_owned),
            None => {
                self.want(CatalogQuery::Schemas);
                Some(name.to_owned())
            }
        }
    }

    /// The relation a FROM item names, when its schema's relations are loaded.
    fn resolve(&mut self, schema: Option<&str>, name: &str) -> Option<RelationRef> {
        let schema = match schema {
            Some(s) => self.schema(s)?,
            None => self.default_schema?.to_owned(),
        };
        let Some(list) = self.tree.relations(self.conn, &schema) else {
            self.want(CatalogQuery::Relations { schema });
            return None;
        };
        let found = pick(list.items().iter().map(|r| r.name()), name)?;
        Some(RelationRef::new(schema.clone(), found))
    }

    /// Columns of every resolvable FROM item matching `prefix`, each name once.
    fn columns<'f>(
        &mut self,
        from: impl IntoIterator<Item = &'f FromItem>,
        prefix: &str,
    ) -> Vec<Candidate> {
        let mut seen: Vec<RelationRef> = Vec::new();
        let mut out: Vec<Candidate> = Vec::new();
        for item in from {
            let Some(rel) = self.resolve(item.schema.as_deref(), &item.name) else {
                continue;
            };
            if seen.contains(&rel) {
                continue;
            }
            let Some(cols) = self.tree.columns(self.conn, &rel) else {
                self.want(CatalogQuery::Columns { rel });
                continue;
            };
            for c in cols {
                if starts(c.name(), prefix) && !out.iter().any(|o| o.label == c.name()) {
                    out.push(self.ident(c.name(), CandidateKind::Column, c.type_name()));
                }
            }
            seen.push(rel);
        }
        sorted(out)
    }

    /// Relations of `schema` (the default one when None) matching `prefix`.
    fn schema_relations(&mut self, schema: Option<&str>, prefix: &str) -> Vec<Candidate> {
        let schema = match schema {
            Some(s) => match self.schema(s) {
                Some(s) => s,
                None => return Vec::new(),
            },
            None => match self.default_schema {
                Some(s) => s.to_owned(),
                None => return Vec::new(),
            },
        };
        let Some(list) = self.tree.relations(self.conn, &schema) else {
            self.want(CatalogQuery::Relations { schema });
            return Vec::new();
        };
        let out = list
            .items()
            .iter()
            .filter(|r| starts(r.name(), prefix))
            .map(|r| self.ident(r.name(), CandidateKind::Relation, kind_detail(r.kind())))
            .collect();
        sorted(out)
    }

    /// Schemas matching `prefix`.
    fn schemas(&mut self, prefix: &str) -> Vec<Candidate> {
        let Some(list) = self.tree.schemas(self.conn) else {
            self.want(CatalogQuery::Schemas);
            return Vec::new();
        };
        let out = list
            .iter()
            .filter(|s| starts(s.name(), prefix))
            .map(|s| self.ident(s.name(), CandidateKind::Schema, "schema"))
            .collect();
        sorted(out)
    }

    fn ident(&self, name: &str, kind: CandidateKind, detail: &str) -> Candidate {
        Candidate {
            label: name.to_owned(),
            insert: quote_ident(self.engine, name),
            kind,
            detail: detail.to_owned(),
        }
    }
}

/// `engine`'s keywords matching `prefix`, uppercase when the prefix holds an uppercase letter;
/// SurrealDB adds its function paths, which keep their own lowercase spelling.
fn keywords(engine: Engine, prefix: &str) -> Vec<Candidate> {
    let upper = prefix.chars().any(char::is_uppercase);
    let sr = engine == Engine::Surrealdb;
    let list = if sr { SURREAL_KEYWORDS } else { KEYWORDS };
    let mut out: Vec<Candidate> = list
        .iter()
        .filter(|k| starts(k, prefix))
        .map(|k| {
            let word = if upper {
                (*k).to_owned()
            } else {
                k.to_lowercase()
            };
            Candidate {
                label: word.clone(),
                insert: word,
                kind: CandidateKind::Keyword,
                detail: "keyword".into(),
            }
        })
        .collect();
    if sr {
        out.extend(
            SURREAL_FUNCTIONS
                .iter()
                .filter(|f| starts(f, prefix))
                .map(|f| Candidate {
                    label: (*f).to_owned(),
                    insert: (*f).to_owned(),
                    kind: CandidateKind::Keyword,
                    detail: "function".into(),
                }),
        );
    }
    out.sort_by(|a, b| a.label.cmp(&b.label));
    out
}

fn sorted(mut v: Vec<Candidate>) -> Vec<Candidate> {
    v.sort_by_cached_key(|c| c.label.to_lowercase());
    v
}

fn starts(name: &str, prefix: &str) -> bool {
    name.to_lowercase().starts_with(&prefix.to_lowercase())
}

/// The name in `names` equal to `want`, else the first equal ignoring case.
fn pick<'n>(names: impl Iterator<Item = &'n str> + Clone, want: &str) -> Option<&'n str> {
    names.clone().find(|n| *n == want).or_else(|| {
        names
            .into_iter()
            .find(|n| n.to_lowercase() == want.to_lowercase())
    })
}

fn kind_detail(kind: RelationKind) -> &'static str {
    match kind {
        RelationKind::Table => "table",
        RelationKind::View => "view",
        RelationKind::MaterializedView => "materialized view",
        RelationKind::ForeignTable => "foreign table",
        _ => "relation",
    }
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum TokKind {
    Word,
    /// A quoted name, unescaped.
    Quoted(String),
    Str,
    Comment,
    Punct(char),
}

#[derive(Debug, Clone)]
pub(crate) struct Tok {
    pub(crate) kind: TokKind,
    pub(crate) range: Range<usize>,
    /// The token runs to its end without a closing delimiter, so a caret at its end is inside.
    pub(crate) open: bool,
}

impl Tok {
    /// The identifier the token spells, when it is a word or quoted name.
    fn name<'s>(&'s self, src: &'s str) -> Option<&'s str> {
        match &self.kind {
            TokKind::Word => Some(&src[self.range.clone()]),
            TokKind::Quoted(n) => Some(n),
            _ => None,
        }
    }

    pub(crate) fn is_word(&self, src: &str, word: &str) -> bool {
        self.kind == TokKind::Word && src[self.range.clone()].eq_ignore_ascii_case(word)
    }
}

fn is_word_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || c == '_' || c == '$' || (!c.is_ascii() && c.is_alphanumeric())
}

/// Splits one statement into words, quoted names, strings, comments and punctuation. Dollar
/// quotes and nested comments are Postgres's, which DuckDB shares (a bracket opens its list);
/// brackets and backticks quote names on SQLite.
/// MySQL is read under the default `sql_mode`: backslash escapes, `"` quotes a string, `#` and
/// `-- ` start line comments, and only backticks quote names.
/// SQL Server mirrors core's T-SQL lexer: `[..]` and `"..."` quote names with the close
/// doubled, `N'..'` is a string, `--` runs to `\n` or `\r`, comments nest, and `@` and `#` are
/// word characters.
/// SurrealDB reads `'..'` and `"..."` as strings with backslash escapes (one `rdubsf` prefix
/// letter joins them), backticks and `⟨..⟩` as names, `--`, `//` and `#` as line comments, and
/// `::` as part of a function path word.
pub(crate) fn lex(engine: Engine, s: &str) -> Vec<Tok> {
    let pg = matches!(engine, Engine::Postgres | Engine::Duckdb);
    let my = matches!(engine, Engine::Mysql);
    let ms = engine == Engine::Mssql;
    let sr = engine == Engine::Surrealdb;
    let word_char = |c: char| is_word_char(c) || (ms && matches!(c, '@' | '#'));
    let b = s.as_bytes();
    let mut toks = Vec::new();
    let mut i = 0;
    while i < s.len() {
        let c = s[i..].chars().next().unwrap_or(' ');
        let start = i;
        let line_comment = if my {
            c == '#' || (s[i..].starts_with("--") && mysql_dashes_comment(b.get(i + 2)))
        } else if sr {
            c == '#' || s[i..].starts_with("--") || s[i..].starts_with("//")
        } else {
            s[i..].starts_with("--")
        };
        let (kind, end, open) = if c.is_whitespace() {
            i += c.len_utf8();
            continue;
        } else if line_comment {
            let end = s[i..]
                .find(|ch| {
                    ch == '\n'
                        || ((ms || sr) && ch == '\r')
                        || (sr && matches!(ch, '\u{85}' | '\u{2028}' | '\u{2029}'))
                })
                .map_or(s.len(), |n| i + n);
            (TokKind::Comment, end, true)
        } else if s[i..].starts_with("/*") {
            let (end, open) = block_comment(s, i, pg || ms);
            (TokKind::Comment, end, open)
        } else if sr && matches!(c, '\'' | '"') {
            let (end, open) = quoted(b, i, c as u8, true, false);
            (TokKind::Str, end, open)
        } else if sr && "rdubsf".contains(c) && matches!(b.get(i + 1), Some(b'\'' | b'"')) {
            // A prefix letter touching the quote types the string: r'..', d'..', u'..' and kin.
            let (end, open) = quoted(b, i + 1, b[i + 1], true, false);
            (TokKind::Str, end, open)
        } else if sr && c == '`' {
            let (end, open) = quoted(b, i, b'`', true, false);
            let inner = &s[i + 1..if open { end } else { end - 1 }];
            (TokKind::Quoted(unescape_backticks(inner)), end, open)
        } else if sr && c == '⟨' {
            let from = i + c.len_utf8();
            match s[from..].find('⟩') {
                Some(n) => (
                    TokKind::Quoted(s[from..from + n].to_owned()),
                    from + n + '⟩'.len_utf8(),
                    false,
                ),
                None => (TokKind::Quoted(s[from..].to_owned()), s.len(), true),
            }
        } else if c == '\'' || (my && c == '"') {
            let (end, open) = quoted(b, i, c as u8, my, true);
            (TokKind::Str, end, open)
        } else if ms && c.eq_ignore_ascii_case(&'n') && b.get(i + 1) == Some(&b'\'') {
            // A lone `N` touching a quote is the Unicode string prefix, part of the string.
            let (end, open) = quoted(b, i + 1, b'\'', false, true);
            (TokKind::Str, end, open)
        } else if c == '"' || (!pg && !ms && c == '`') || (ms && c == '[') {
            let close = if c == '[' { ']' } else { c };
            let (end, open) = quoted(b, i, close as u8, false, true);
            let inner = &s[i + 1..if open { end } else { end - 1 }];
            let d = close.to_string();
            (TokKind::Quoted(inner.replace(&d.repeat(2), &d)), end, open)
        } else if !pg && !my && !sr && c == '[' {
            match s[i..].find(']') {
                Some(n) => (
                    TokKind::Quoted(s[i + 1..i + n].to_owned()),
                    i + n + 1,
                    false,
                ),
                None => (TokKind::Quoted(s[i + 1..].to_owned()), s.len(), true),
            }
        } else if let Some(delim) = dollar_tag(s, i).filter(|_| pg) {
            match s[i + delim.len()..].find(delim) {
                Some(n) => (TokKind::Str, i + delim.len() + n + delim.len(), false),
                None => (TokKind::Str, s.len(), true),
            }
        } else if word_char(c) {
            let word_end = |j: usize| {
                s[j..]
                    .find(|ch: char| !word_char(ch))
                    .map_or(s.len(), |n| j + n)
            };
            let mut end = word_end(i);
            // A SurrealQL function path such as `string::lowercase` reads as one word.
            while sr && s[end..].starts_with("::") {
                end = word_end(end + 2);
            }
            (TokKind::Word, end, false)
        } else {
            (TokKind::Punct(c), i + c.len_utf8(), false)
        };
        toks.push(Tok {
            kind,
            range: start..end,
            open,
        });
        i = end;
    }
    toks
}

/// Whether `--` followed by `next` opens a MySQL comment: only whitespace, a control
/// character or the end of the text may follow the dashes.
fn mysql_dashes_comment(next: Option<&u8>) -> bool {
    next.is_none_or(|c| c.is_ascii_whitespace() || c.is_ascii_control())
}

/// The end of the quoted run at `i` closed by `q` (a doubled `q` escapes it when `doubled`, as
/// does a backslash when `backslash`), and whether it is open.
fn quoted(b: &[u8], i: usize, q: u8, backslash: bool, doubled: bool) -> (usize, bool) {
    let mut j = i + 1;
    while j < b.len() {
        if backslash && b[j] == b'\\' {
            j += 2;
            continue;
        }
        if b[j] == q {
            if doubled && b.get(j + 1) == Some(&q) {
                j += 2;
                continue;
            }
            return (j + 1, false);
        }
        j += 1;
    }
    (b.len(), true)
}

/// A SurrealQL backtick name with its `` \` `` and `\\` escapes undone.
fn unescape_backticks(inner: &str) -> String {
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        match (c, chars.clone().next()) {
            ('\\', Some(n @ ('`' | '\\'))) => {
                out.push(n);
                chars.next();
            }
            _ => out.push(c),
        }
    }
    out
}

/// The end of the block comment at `i`, and whether it is open; Postgres nests them.
fn block_comment(s: &str, i: usize, nest: bool) -> (usize, bool) {
    let mut depth = 0;
    let mut j = i;
    while j < s.len() {
        if s[j..].starts_with("/*") && (nest || depth == 0) {
            depth += 1;
            j += 2;
        } else if s[j..].starts_with("*/") {
            depth -= 1;
            j += 2;
            if depth == 0 {
                return (j, false);
            }
        } else {
            j += s[j..].chars().next().map_or(1, char::len_utf8);
        }
    }
    (s.len(), true)
}

/// The `$tag$` opening a dollar quote at `i`, if one does.
fn dollar_tag(s: &str, i: usize) -> Option<&str> {
    let rest = s[i..].strip_prefix('$')?;
    let n = rest.find('$')?;
    let tag = &rest[..n];
    let ok = tag.is_empty()
        || (!tag.starts_with(|c: char| c.is_ascii_digit())
            && tag.chars().all(|c| c != '$' && is_word_char(c)));
    ok.then(|| &s[i..i + n + 2])
}

/// Where the caret sits: the identifier prefix, its qualifier, and whether a relation is expected.
struct Site {
    word: Range<usize>,
    qualifier: Option<String>,
    relation_context: bool,
}

/// Reads the caret's site, or None where nothing should complete: inside a string, comment or
/// quoted name, with no identifier prefix (except right after `.`), or on a number.
fn site(toks: &[Tok], src: &str, off: usize) -> Option<Site> {
    let inside = toks.iter().any(|t| {
        t.kind != TokKind::Word
            && t.range.start < off
            && (off < t.range.end || (t.open && off == t.range.end))
    });
    if inside {
        return None;
    }
    let word_ix = toks
        .iter()
        .position(|t| t.kind == TokKind::Word && t.range.start < off && off <= t.range.end);
    // `before` is the index of the first token preceding the prefix.
    let (word, before) = match word_ix {
        Some(ix) => {
            if src[toks[ix].range.clone()].starts_with(|c: char| c.is_ascii_digit()) {
                return None;
            }
            (toks[ix].range.start..off, ix)
        }
        None => (
            off..off,
            toks.iter().take_while(|t| t.range.end <= off).count(),
        ),
    };
    let prev = before.checked_sub(1).map(|i| &toks[i]);
    let dot = prev.filter(|t| t.kind == TokKind::Punct('.') && t.range.end == word.start);
    let (qualifier, ctx_ix) = match dot {
        Some(d) => {
            let q = toks[..before - 1]
                .last()
                .filter(|t| t.range.end == d.range.start)
                .and_then(|t| t.name(src))?;
            (Some(q.to_owned()), before - 2)
        }
        None if word.is_empty() => return None,
        None => (None, before),
    };
    let relation_context = toks[..ctx_ix]
        .iter()
        .rev()
        .find(|t| t.kind != TokKind::Comment)
        .is_some_and(|t| RELATION_KEYWORDS.iter().any(|k| t.is_word(src, k)));
    Some(Site {
        word,
        qualifier,
        relation_context,
    })
}

/// One relation a FROM or JOIN names, with the alias it is declared under.
struct FromItem {
    schema: Option<String>,
    name: String,
    alias: Option<String>,
}

/// The `name` or `schema.name` items after each FROM and JOIN, skipping the one the caret is in.
fn from_items(toks: &[Tok], src: &str, off: usize) -> Vec<FromItem> {
    let t: Vec<&Tok> = toks.iter().filter(|t| t.kind != TokKind::Comment).collect();
    let is_keyword = |tok: &Tok| KEYWORDS.iter().any(|k| tok.is_word(src, k));
    let punct = |i: usize, c: char| t.get(i).is_some_and(|x| x.kind == TokKind::Punct(c));
    let mut out = Vec::new();
    let mut i = 0;
    while i < t.len() {
        if !(t[i].is_word(src, "FROM") || t[i].is_word(src, "JOIN")) {
            i += 1;
            continue;
        }
        i += 1;
        // One item per pass: a name, an optional `.name`, an optional alias, then `,` or stop.
        while let Some(first) = t.get(i).and_then(|x| x.name(src).map(|n| (x, n))) {
            let (tok, name) = first;
            let start = tok.range.start;
            let mut end = tok.range.end;
            i += 1;
            let mut item = Some((None, name.to_owned()));
            if punct(i, '.') {
                end = t[i].range.end;
                i += 1;
                item = match t.get(i).and_then(|x| x.name(src).map(|n| (x, n))) {
                    Some((x, n)) => {
                        end = x.range.end;
                        i += 1;
                        Some((Some(name.to_owned()), n.to_owned()))
                    }
                    None => None,
                };
            }
            // The alias follows `AS`, or stands bare when it is a name and not a keyword.
            let mut alias = None;
            if t.get(i).is_some_and(|x| x.is_word(src, "AS")) {
                alias = t.get(i + 1).and_then(|x| x.name(src));
                i += 2;
            } else if let Some(x) = t.get(i).filter(|x| !is_keyword(x)) {
                alias = x.name(src);
                if alias.is_some() {
                    i += 1;
                }
            }
            if !(start <= off && off <= end)
                && let Some((schema, name)) = item
            {
                out.push(FromItem {
                    schema,
                    name,
                    alias: alias.map(str::to_owned),
                });
            }
            if !punct(i, ',') {
                break;
            }
            i += 1;
        }
    }
    out
}
