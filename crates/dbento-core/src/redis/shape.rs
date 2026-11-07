//! RESP3 replies as grid rows. A scalar is one `value` cell, a map is `key` and `value` rows,
//! an array of equal short arrays is numbered columns, and anything nested is compact JSON.

use std::borrow::Cow;

use ::redis::Value;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde_json::{Map, Value as Json};

use crate::numtext::float_text;
use crate::value::{BatchBuilder, CellRef, ColumnInfo, ResultBatch};

/// The widest row of arrays that still splits into numbered columns.
const MAX_NUMBERED: usize = 16;

/// A reply as columns and rows.
#[derive(Debug)]
pub(crate) struct Shaped {
    pub(crate) columns: Vec<ColumnInfo>,
    pub(crate) batches: Vec<ResultBatch>,
    /// Rows delivered. The stream counts rows itself; only the tests read this.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) rows: u64,
    /// More elements existed than `max_rows` let through.
    pub(crate) truncated: bool,
}

/// A reply that is not rows.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum ShapeError {
    /// A top-level error reply: its first word, and the rest, which may be empty.
    #[error("{code}{}{message}", if message.is_empty() { "" } else { " " })]
    Server { code: String, message: String },
    #[error("the server sent a push reply")]
    Push,
    #[error("the server sent a reply type Dbento does not know")]
    Unknown,
}

/// Turns one reply into rows, stopping before element `max_rows` of the outer aggregate.
pub(crate) fn reply_to_rows(value: Value, max_rows: Option<u64>) -> Result<Shaped, ShapeError> {
    let mut value = value;
    while let Value::Attribute { data, .. } = value {
        value = *data;
    }
    match value {
        Value::Okay => Ok(Shaped {
            columns: Vec::new(),
            batches: Vec::new(),
            rows: 0,
            truncated: false,
        }),
        Value::ServerError(e) => Err(ShapeError::Server {
            code: e.code().to_owned(),
            message: e.details().unwrap_or("").to_owned(),
        }),
        Value::Push { .. } => Err(ShapeError::Push),
        Value::Map(pairs) => {
            let rows = pairs.iter().map(|(k, v)| vec![k, v]);
            fill(&["key", "value"], rows, max_rows)
        }
        Value::Array(items) | Value::Set(items) => match numbered_width(&items) {
            Some(width) => {
                let names: Vec<String> = (1..=width).map(|i| i.to_string()).collect();
                let names: Vec<&str> = names.iter().map(String::as_str).collect();
                let rows = items.iter().map(|row| match row {
                    Value::Array(cells) | Value::Set(cells) => cells.iter().collect(),
                    _ => unreachable!("numbered_width admits only arrays and sets"),
                });
                fill(&names, rows, max_rows)
            }
            None => fill(&["value"], items.iter().map(|v| vec![v]), max_rows),
        },
        scalar @ (Value::Nil
        | Value::Int(_)
        | Value::BulkString(_)
        | Value::SimpleString(_)
        | Value::VerbatimString { .. }
        | Value::Double(_)
        | Value::Boolean(_)
        | Value::BigNumber(_)) => fill(&["value"], std::iter::once(vec![&scalar]), max_rows),
        _ => Err(ShapeError::Unknown),
    }
}

/// The shared width when every element is an array or set of 1 to 16 elements, all one length.
fn numbered_width(items: &[Value]) -> Option<usize> {
    let mut width = None;
    for item in items {
        let (Value::Array(cells) | Value::Set(cells)) = item else {
            return None;
        };
        if !(1..=MAX_NUMBERED).contains(&cells.len()) || width.is_some_and(|w| w != cells.len()) {
            return None;
        }
        width = Some(cells.len());
    }
    width
}

/// The kind a column has seen so far: no non-null cell yet, one shared kind, or several.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    None,
    One(&'static str),
    Mixed,
}

impl Kind {
    fn add(self, kind: &'static str) -> Self {
        match self {
            Kind::None => Kind::One(kind),
            Kind::One(k) if k == kind => self,
            _ => Kind::Mixed,
        }
    }

    fn name(self) -> &'static str {
        match self {
            Kind::None => "",
            Kind::One(k) => k,
            Kind::Mixed => "mixed",
        }
    }
}

/// Pushes each row's cells into batches under `max_rows`, tracking each column's kind.
fn fill<'a>(
    names: &[&str],
    rows: impl Iterator<Item = Vec<&'a Value>>,
    max_rows: Option<u64>,
) -> Result<Shaped, ShapeError> {
    let mut builder = BatchBuilder::new(names.len());
    let mut batches = Vec::new();
    let mut kinds = vec![Kind::None; names.len()];
    let mut delivered = 0u64;
    let mut truncated = false;
    for row in rows {
        if max_rows == Some(delivered) {
            truncated = true;
            break;
        }
        let cells = row.into_iter().map(cell).collect::<Result<Vec<_>, _>>()?;
        for (kind, c) in kinds.iter_mut().zip(&cells) {
            if let Some(k) = c.kind {
                *kind = kind.add(k);
            }
        }
        let refs: Vec<CellRef<'_>> = cells.iter().map(Cell::as_ref).collect();
        batches.extend(builder.push_typed_row(&refs, |i| cells[i].number));
        if builder.is_full() {
            batches.push(builder.finish());
        }
        delivered += 1;
    }
    if !builder.is_empty() {
        batches.push(builder.finish());
    }
    let columns = names
        .iter()
        .zip(kinds)
        .map(|(name, kind)| ColumnInfo::new(*name, kind.name(), None))
        .collect();
    Ok(Shaped {
        columns,
        batches,
        rows: delivered,
        truncated,
    })
}

/// One grid cell before it is pushed: its content, its kind (none for null) and its number bit.
struct Cell<'a> {
    content: Content<'a>,
    kind: Option<&'static str>,
    number: bool,
}

enum Content<'a> {
    Null,
    Text(Cow<'a, str>),
    Bytes(&'a [u8]),
}

impl Cell<'_> {
    fn as_ref(&self) -> CellRef<'_> {
        match &self.content {
            Content::Null => CellRef::Null,
            Content::Text(s) => CellRef::Text(s),
            Content::Bytes(b) => CellRef::Bytes(b),
        }
    }
}

fn text<'a>(s: Cow<'a, str>, kind: &'static str, number: bool) -> Cell<'a> {
    Cell {
        content: Content::Text(s),
        kind: Some(kind),
        number,
    }
}

fn cell(value: &Value) -> Result<Cell<'_>, ShapeError> {
    Ok(match value {
        Value::Nil => Cell {
            content: Content::Null,
            kind: None,
            number: false,
        },
        Value::Int(n) => text(n.to_string().into(), "integer", true),
        Value::BulkString(b) => match std::str::from_utf8(b) {
            Ok(s) => text(s.into(), "string", false),
            Err(_) => Cell {
                content: Content::Bytes(b),
                kind: Some("bytes"),
                number: false,
            },
        },
        Value::SimpleString(s) | Value::VerbatimString { text: s, .. } => {
            text(s.into(), "string", false)
        }
        Value::Okay => text("OK".into(), "string", false),
        Value::Double(f) => text(double_text(*f).into(), "double", true),
        Value::Boolean(b) => text(b.to_string().into(), "boolean", false),
        Value::BigNumber(digits) => text(String::from_utf8_lossy(digits), "bignumber", true),
        Value::ServerError(e) => text(error_text(e).into(), "error", false),
        Value::Array(_) | Value::Set(_) => text(json(value)?.to_string().into(), "array", false),
        Value::Map(_) => text(json(value)?.to_string().into(), "map", false),
        Value::Attribute { data, .. } => cell(data)?,
        Value::Push { .. } => return Err(ShapeError::Push),
        _ => return Err(ShapeError::Unknown),
    })
}

/// A nested value as JSON: bytes and errors as tagged objects, and every number JSON cannot
/// hold exactly (a non-finite double, a big number) as a string.
pub(crate) fn json(value: &Value) -> Result<Json, ShapeError> {
    Ok(match value {
        Value::Nil => Json::Null,
        Value::Int(n) => (*n).into(),
        Value::BulkString(b) => match std::str::from_utf8(b) {
            Ok(s) => s.into(),
            Err(_) => Json::Object(Map::from_iter([(
                "$bytes".into(),
                STANDARD.encode(b).into(),
            )])),
        },
        Value::SimpleString(s) | Value::VerbatimString { text: s, .. } => s.as_str().into(),
        Value::Okay => "OK".into(),
        Value::Double(f) => {
            serde_json::Number::from_f64(*f).map_or_else(|| double_text(*f).into(), Json::Number)
        }
        Value::Boolean(b) => (*b).into(),
        Value::BigNumber(digits) => String::from_utf8_lossy(digits).into(),
        Value::ServerError(e) => {
            Json::Object(Map::from_iter([("$error".into(), error_text(e).into())]))
        }
        Value::Array(items) | Value::Set(items) => {
            Json::Array(items.iter().map(json).collect::<Result<_, _>>()?)
        }
        Value::Map(pairs) => {
            let mut object = Map::new();
            for (k, v) in pairs {
                object.insert(json_key(k)?, json(v)?);
            }
            Json::Object(object)
        }
        Value::Attribute { data, .. } => json(data)?,
        Value::Push { .. } => return Err(ShapeError::Push),
        _ => return Err(ShapeError::Unknown),
    })
}

/// A map key as a JSON object key: a string as itself, anything else as its compact JSON.
pub(crate) fn json_key(key: &Value) -> Result<String, ShapeError> {
    Ok(match json(key)? {
        Json::String(s) => s,
        other => other.to_string(),
    })
}

pub(crate) fn double_text(f: f64) -> String {
    let mut s = String::new();
    float_text(
        &mut s,
        f.is_nan(),
        f.is_infinite(),
        f.is_sign_negative(),
        format_args!("{f:e}"),
    );
    s
}

/// An error as `CODE message`, or the bare code when the server sent no message.
fn error_text(e: &::redis::ServerError) -> String {
    match e.details() {
        Some(details) if !details.is_empty() => format!("{} {details}", e.code()),
        _ => e.code().to_owned(),
    }
}

#[cfg(test)]
mod tests;
