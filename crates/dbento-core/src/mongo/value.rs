//! Mongosh values into BSON: object and array literals, strings, numbers, regular
//! expressions and the shell's type helpers such as `ObjectId(..)` and `ISODate(..)`.
//! Anything else JavaScript would accept is refused with [`NOT_JS`].

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use bson::oid::ObjectId;
use bson::raw::CString;
use bson::spec::BinarySubtype;
use bson::{Binary, Bson, DateTime, Decimal128, Document, Regex, Timestamp, Uuid};

use super::lex::{Kind, Tok, regex_parts, string_value};

pub(crate) const NOT_JS: &str = "Dbento runs MongoDB shell statements, not JavaScript";

// BSON's own nesting limit for documents and arrays.
const MAX_DEPTH: usize = 100;

const HELPERS: &[&str] = &[
    "ObjectId",
    "ISODate",
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

/// A parse failure: its message and the absolute byte offset it points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ParseError {
    pub(crate) message: String,
    pub(crate) at: usize,
}

impl ParseError {
    pub(crate) fn new(message: String, at: usize) -> Self {
        Self { message, at }
    }
}

fn not_js(at: usize) -> ParseError {
    ParseError::new(NOT_JS.to_owned(), at)
}

/// A read position over one statement's tokens, which carry absolute ranges into `src`.
pub(crate) struct Cursor<'a> {
    src: &'a str,
    toks: &'a [Tok],
    pos: usize,
}

impl<'a> Cursor<'a> {
    pub(crate) fn new(src: &'a str, toks: &'a [Tok]) -> Self {
        Self { src, toks, pos: 0 }
    }

    pub(crate) fn peek(&self) -> Option<&'a Tok> {
        self.peek_nth(0)
    }

    pub(crate) fn peek_nth(&self, n: usize) -> Option<&'a Tok> {
        self.toks.get(self.pos + n)
    }

    pub(crate) fn bump(&mut self) -> Option<&'a Tok> {
        let tok = self.peek()?;
        self.pos += 1;
        Some(tok)
    }

    pub(crate) fn text(&self, t: &Tok) -> &'a str {
        &self.src[t.range.clone()]
    }

    pub(crate) fn at_end(&self) -> bool {
        self.pos >= self.toks.len()
    }

    /// The last token's end, or 0 with no tokens: where an "expected" error at the end points.
    pub(crate) fn end_offset(&self) -> usize {
        self.toks.last().map_or(0, |t| t.range.end)
    }

    fn eat(&mut self, punct: u8) -> bool {
        let hit = self.peek().is_some_and(|t| t.kind == Kind::Punct(punct));
        self.pos += usize::from(hit);
        hit
    }
}

/// One value at the cursor, which is left on the token after it.
pub(crate) fn value(c: &mut Cursor<'_>) -> Result<Bson, ParseError> {
    value_at(c, 0)
}

fn value_at(c: &mut Cursor<'_>, depth: usize) -> Result<Bson, ParseError> {
    let Some(tok) = c.peek() else {
        return Err(ParseError::new(
            "expected a value".to_owned(),
            c.end_offset(),
        ));
    };
    let at = tok.range.start;
    // Objects, arrays and helper calls each open a level, so the limit bounds the recursion
    // while a scalar at the deepest level still parses.
    let opens_level = match tok.kind {
        Kind::Punct(b'{' | b'[') => true,
        Kind::Ident => {
            c.text(tok) == "new" || c.peek_nth(1).is_some_and(|t| t.kind == Kind::Punct(b'('))
        }
        Kind::Str | Kind::Num | Kind::Regex | Kind::Punct(_) => false,
    };
    if depth == MAX_DEPTH && opens_level {
        return Err(ParseError::new(
            "nested deeper than 100 levels".to_owned(),
            at,
        ));
    }
    match tok.kind {
        Kind::Ident => return ident(c, depth),
        Kind::Punct(b',' | b';' | b':' | b'}' | b']' | b')') => {
            return Err(ParseError::new("expected a value".to_owned(), at));
        }
        Kind::Str | Kind::Num | Kind::Regex | Kind::Punct(_) => {}
    }
    c.bump();
    match tok.kind {
        Kind::Str => Ok(Bson::String(string_value(c.text(tok)))),
        Kind::Num => Ok(number(c.text(tok), false)),
        Kind::Regex => {
            let (pattern, flags) = regex_parts(c.text(tok));
            regex(pattern, flags, at)
        }
        // A unary minus binds only to a number literal; the error points at the minus.
        Kind::Punct(b'-') => match c.peek() {
            Some(num) if num.kind == Kind::Num => {
                c.bump();
                Ok(number(c.text(num), true))
            }
            _ => Err(not_js(at)),
        },
        Kind::Punct(b'{') => object(c, depth + 1),
        Kind::Punct(b'[') => array(c, depth + 1),
        Kind::Ident | Kind::Punct(_) => Err(not_js(at)),
    }
}

// The minus is applied before the range check, so `-2147483648` stays Int32.
fn number(text: &str, negative: bool) -> Bson {
    let v = text.parse::<f64>().unwrap_or(f64::NAN);
    let v = if negative { -v } else { v };
    let int32 = f64::from(i32::MIN)..=f64::from(i32::MAX);
    if v.fract() == 0.0 && int32.contains(&v) {
        Bson::Int32(v as i32)
    } else {
        Bson::Double(v)
    }
}

// After an element: a `,` (false) or the closer (true). An operator there is JavaScript;
// anything else is a missing separator.
pub(crate) fn separator(c: &mut Cursor<'_>, close: u8) -> Result<bool, ParseError> {
    let expected = |at| ParseError::new(format!("expected `,` or `{}`", char::from(close)), at);
    let Some(tok) = c.bump() else {
        return Err(expected(c.end_offset()));
    };
    match tok.kind {
        Kind::Punct(b',') => Ok(false),
        Kind::Punct(b) if b == close => Ok(true),
        Kind::Punct(b) if !b",;:}])".contains(&b) => Err(not_js(tok.range.start)),
        Kind::Ident | Kind::Str | Kind::Num | Kind::Regex | Kind::Punct(_) => {
            Err(expected(tok.range.start))
        }
    }
}

// Keys are identifiers, strings or a number's text; `insert` keeps a repeated key's first
// position and takes its last value.
fn object(c: &mut Cursor<'_>, depth: usize) -> Result<Bson, ParseError> {
    let mut doc = Document::new();
    loop {
        if c.eat(b'}') {
            return Ok(Bson::Document(doc));
        }
        let Some(key_tok) = c.peek() else {
            return Err(ParseError::new("expected a key".to_owned(), c.end_offset()));
        };
        let key = match key_tok.kind {
            Kind::Ident | Kind::Num => c.text(key_tok).to_owned(),
            Kind::Str => string_value(c.text(key_tok)),
            Kind::Regex | Kind::Punct(_) => {
                return Err(ParseError::new(
                    "expected a key".to_owned(),
                    key_tok.range.start,
                ));
            }
        };
        c.bump();
        match c.peek() {
            Some(t) if t.kind == Kind::Punct(b':') => {
                c.bump();
            }
            // `{a}` and `{a, b}` are JavaScript's shorthand properties.
            Some(t) if matches!(t.kind, Kind::Punct(b',' | b'}')) => {
                return Err(not_js(key_tok.range.start));
            }
            t => {
                let at = t.map_or(c.end_offset(), |t| t.range.start);
                return Err(ParseError::new("expected `:`".to_owned(), at));
            }
        }
        doc.insert(key, value_at(c, depth)?);
        if separator(c, b'}')? {
            return Ok(Bson::Document(doc));
        }
    }
}

fn array(c: &mut Cursor<'_>, depth: usize) -> Result<Bson, ParseError> {
    let mut items = Vec::new();
    loop {
        if c.eat(b']') {
            return Ok(Bson::Array(items));
        }
        items.push(value_at(c, depth)?);
        if separator(c, b']')? {
            return Ok(Bson::Array(items));
        }
    }
}

// `true`, `false`, `null`, `new Date(..)` or a known helper called with parentheses.
fn ident(c: &mut Cursor<'_>, depth: usize) -> Result<Bson, ParseError> {
    let Some(tok) = c.bump() else {
        return Err(ParseError::new(
            "expected a value".to_owned(),
            c.end_offset(),
        ));
    };
    let called = |c: &Cursor<'_>, n| c.peek_nth(n).is_some_and(|t| t.kind == Kind::Punct(b'('));
    match c.text(tok) {
        "true" => Ok(Bson::Boolean(true)),
        "false" => Ok(Bson::Boolean(false)),
        "null" => Ok(Bson::Null),
        "new" => match c.peek() {
            Some(date) if c.text(date) == "Date" && called(c, 1) => {
                c.bump();
                let args = call_args(c, depth + 1)?;
                helper("new Date", &args, date.range.start)
            }
            next => Err(not_js(next.unwrap_or(tok).range.start)),
        },
        name if HELPERS.contains(&name) && called(c, 0) => {
            let args = call_args(c, depth + 1)?;
            helper(name, &args, tok.range.start)
        }
        _ => Err(not_js(tok.range.start)),
    }
}

struct Arg<'a> {
    value: Bson,
    text: &'a str,
}

// The parenthesised arguments, each with its source text for helpers that read a number as written.
fn call_args<'a>(c: &mut Cursor<'a>, depth: usize) -> Result<Vec<Arg<'a>>, ParseError> {
    c.bump();
    let mut args = Vec::new();
    loop {
        if c.eat(b')') {
            return Ok(args);
        }
        let first = c.pos;
        let value = value_at(c, depth)?;
        let text = &c.src[c.toks[first].range.start..c.toks[c.pos - 1].range.end];
        args.push(Arg { value, text });
        if separator(c, b')')? {
            return Ok(args);
        }
    }
}

fn helper(name: &str, args: &[Arg<'_>], at: usize) -> Result<Bson, ParseError> {
    let values: Vec<&Bson> = args.iter().map(|a| &a.value).collect();
    let usage = match name {
        "ObjectId" => "takes a 24-digit hex string",
        "ISODate" => "takes an ISO-8601 date string",
        "new Date" => "takes an ISO-8601 date string or milliseconds",
        "NumberInt" | "Int32" => "takes a 32-bit integer",
        "NumberLong" | "Long" => "takes a 64-bit integer",
        "Double" => "takes a number",
        "NumberDecimal" | "Decimal128" => "takes a decimal string or number",
        "UUID" => "takes a UUID string",
        "BinData" => "takes a subtype from 0 to 255 and a base64 string",
        "Timestamp" => "takes `t` and `i`, each from 0 to 4294967295",
        "MinKey" | "MaxKey" => "takes no arguments",
        _ => "takes a pattern string and an optional flags string",
    };
    let bad = || ParseError::new(format!("`{name}` {usage}"), at);
    let built = match (name, values.as_slice()) {
        ("ObjectId", []) => Some(Bson::ObjectId(ObjectId::new())),
        ("ObjectId", [Bson::String(s)]) => ObjectId::parse_str(s).ok().map(Bson::ObjectId),
        ("ISODate" | "new Date", []) => Some(Bson::DateTime(DateTime::now())),
        ("ISODate" | "new Date", [Bson::String(s)]) => iso_date(s).map(Bson::DateTime),
        ("new Date", [ms]) => integral(ms).map(|ms| Bson::DateTime(DateTime::from_millis(ms))),
        ("NumberInt" | "Int32", [Bson::String(s)]) => s.parse().ok().map(Bson::Int32),
        ("NumberInt" | "Int32", [n]) => integral(n)
            .and_then(|n| i32::try_from(n).ok())
            .map(Bson::Int32),
        ("NumberLong" | "Long", [Bson::String(s)]) => s.parse().ok().map(Bson::Int64),
        ("NumberLong" | "Long", [n]) => integral(n).map(Bson::Int64),
        ("Double", [Bson::String(s)]) => s.parse().ok().map(Bson::Double),
        ("Double", [Bson::Int32(n)]) => Some(Bson::Double(f64::from(*n))),
        ("Double", [Bson::Double(n)]) => Some(Bson::Double(*n)),
        ("NumberDecimal" | "Decimal128", [Bson::String(s)]) => decimal(s),
        // A number argument is read as written, so `NumberDecimal(1.10)` keeps its trailing zero.
        ("NumberDecimal" | "Decimal128", [Bson::Int32(_) | Bson::Double(_)]) => {
            decimal(&args[0].text.split_whitespace().collect::<String>())
        }
        ("UUID", []) => Some(Bson::Binary(Binary::from_uuid(Uuid::new()))),
        ("UUID", [Bson::String(s)]) => Uuid::parse_str(s)
            .ok()
            .map(|u| Bson::Binary(Binary::from_uuid(u))),
        ("BinData", [subtype, Bson::String(b64)]) => {
            let subtype = integral(subtype).and_then(|n| u8::try_from(n).ok());
            let bytes = STANDARD.decode(b64).ok();
            subtype.zip(bytes).map(|(subtype, bytes)| {
                Bson::Binary(Binary {
                    subtype: BinarySubtype::from(subtype),
                    bytes,
                })
            })
        }
        ("Timestamp", [t, i]) => timestamp(t, i),
        ("Timestamp", [Bson::Document(d)]) if d.len() == 2 => d
            .get("t")
            .zip(d.get("i"))
            .and_then(|(t, i)| timestamp(t, i)),
        ("MinKey", []) => Some(Bson::MinKey),
        ("MaxKey", []) => Some(Bson::MaxKey),
        ("RegExp", [Bson::String(p)]) => return regex(p, "", at),
        ("RegExp", [Bson::String(p), Bson::String(f)]) => return regex(p, f, at),
        _ => None,
    };
    built.ok_or_else(bad)
}

// An integer-valued number in i64's range.
fn integral(n: &Bson) -> Option<i64> {
    #[allow(
        clippy::wildcard_enum_match_arm,
        reason = "Bson has too many variants to list; the rest share one arm"
    )]
    match *n {
        Bson::Int32(n) => Some(n.into()),
        Bson::Int64(n) => Some(n),
        Bson::Double(f) if f.fract() == 0.0 && (-9.2e18..9.2e18).contains(&f) => Some(f as i64),
        _ => None,
    }
}

fn decimal(s: &str) -> Option<Bson> {
    s.parse::<Decimal128>().ok().map(Bson::Decimal128)
}

fn timestamp(t: &Bson, i: &Bson) -> Option<Bson> {
    let u32_of = |n| integral(n).and_then(|n| u32::try_from(n).ok());
    let (time, increment) = u32_of(t).zip(u32_of(i))?;
    Some(Bson::Timestamp(Timestamp { time, increment }))
}

// Mongosh's shapes, normalised to RFC 3339 so bson judges the calendar: a bare date is midnight
// UTC, a time without a zone is UTC, and later clock parts are optional.
fn iso_date(s: &str) -> Option<DateTime> {
    let mut rest = s;
    let year = digits(&mut rest, 4)?;
    let dashed = eat(&mut rest, "-");
    let month = digits(&mut rest, 2)?;
    if dashed && !eat(&mut rest, "-") {
        return None;
    }
    let day = digits(&mut rest, 2)?;
    let (mut hour, mut minute, mut second, mut fraction) = ("00", "00", "00", "");
    let mut zone = "Z".to_owned();
    if eat(&mut rest, "T") || eat(&mut rest, " ") {
        hour = digits(&mut rest, 2)?;
        if let Some(m) = clock_part(&mut rest) {
            minute = m;
            if let Some(sec) = clock_part(&mut rest) {
                second = sec;
                fraction = take_fraction(&mut rest);
            }
        }
        if !eat(&mut rest, "Z")
            && let Some(sign) = rest.get(..1).filter(|s| "+-".contains(s))
        {
            rest = &rest[1..];
            let hours = digits(&mut rest, 2)?;
            eat(&mut rest, ":");
            zone = format!("{sign}{hours}:{}", digits(&mut rest, 2)?);
        }
    }
    if !rest.is_empty() {
        return None;
    }
    let full = format!("{year}-{month}-{day}T{hour}:{minute}:{second}{fraction}{zone}");
    DateTime::parse_rfc3339_str(full).ok()
}

fn eat(rest: &mut &str, prefix: &str) -> bool {
    rest.strip_prefix(prefix).map(|r| *rest = r).is_some()
}

// Exactly `n` leading ASCII digits, consumed only on a match.
fn digits<'a>(rest: &mut &'a str, n: usize) -> Option<&'a str> {
    let head = rest
        .get(..n)
        .filter(|h| h.bytes().all(|b| b.is_ascii_digit()))?;
    *rest = &rest[n..];
    Some(head)
}

// Two digits with an optional colon before them; a lone colon stays behind to be refused.
fn clock_part<'a>(rest: &mut &'a str) -> Option<&'a str> {
    let mut after = rest.strip_prefix(':').unwrap_or(rest);
    let part = digits(&mut after, 2)?;
    *rest = after;
    Some(part)
}

// A `.` and its digits, or nothing when no digit follows the dot.
fn take_fraction<'a>(rest: &mut &'a str) -> &'a str {
    let count = rest
        .strip_prefix('.')
        .map_or(0, |r| r.bytes().take_while(u8::is_ascii_digit).count());
    let len = if count == 0 { 0 } else { count + 1 };
    let (fraction, after) = rest.split_at(len);
    *rest = after;
    fraction
}

// BSON regex options are a sorted subset of `ilmsux`.
fn regex(pattern: &str, flags: &str, at: usize) -> Result<Bson, ParseError> {
    if let Some(flag) = flags.chars().find(|f| !"ilmsux".contains(*f)) {
        let message = format!("regex flag `{flag}` has no BSON equivalent");
        return Err(ParseError::new(message, at));
    }
    let mut sorted: Vec<char> = flags.chars().collect();
    sorted.sort_unstable();
    let nul = || ParseError::new("regex pattern holds a NUL byte".to_owned(), at);
    let pattern = CString::try_from(pattern).map_err(|_| nul())?;
    let options = CString::try_from(sorted.into_iter().collect::<String>()).map_err(|_| nul())?;
    Ok(Bson::RegularExpression(Regex { pattern, options }))
}

#[cfg(test)]
mod tests;
