//! Turns decoded values into cells: bare text at the top level, where the grid shows a value by
//! itself, and compact SurrealQL when nested, where its literal prefix tells the types apart.

use std::borrow::Cow;
use std::fmt::Write as _;
use std::ops::Bound;

use super::decode::Wire;
use super::lex::{quote_ident, quote_string};
use crate::numtext::civil;
use crate::value::CellRef;
use crate::workspace::is_identifier;

const SECONDS_PER_DAY: i64 = 86_400;
const NANOS_PER_SECOND: u128 = 1_000_000_000;

/// SurrealQL's duration units, largest first, in nanoseconds.
const DURATION_UNITS: [(u128, &str); 9] = [
    (365 * 86_400 * NANOS_PER_SECOND, "y"),
    (7 * 86_400 * NANOS_PER_SECOND, "w"),
    (86_400 * NANOS_PER_SECOND, "d"),
    (3_600 * NANOS_PER_SECOND, "h"),
    (60 * NANOS_PER_SECOND, "m"),
    (NANOS_PER_SECOND, "s"),
    (1_000_000, "ms"),
    (1_000, "µs"),
    (1, "ns"),
];

/// One mapped cell; `Number` is text that also sets the number bit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Cell<'w> {
    Null,
    Text(Cow<'w, str>),
    Number(Cow<'w, str>),
    Bytes(&'w [u8]),
}

impl Cell<'_> {
    /// The borrowed view a batch stores.
    pub(crate) fn as_ref(&self) -> CellRef<'_> {
        match self {
            Cell::Null => CellRef::Null,
            Cell::Text(text) | Cell::Number(text) => CellRef::Text(text),
            Cell::Bytes(bytes) => CellRef::Bytes(bytes),
        }
    }

    /// Whether the cell sets the number bit.
    pub(crate) fn is_number(&self) -> bool {
        matches!(self, Cell::Number(_))
    }
}

/// The cell for a top-level value.
pub(crate) fn cell(v: &Wire) -> Cell<'_> {
    match v {
        Wire::None | Wire::Null | Wire::Undefined => Cell::Null,
        Wire::Text(text) | Wire::Table(text) => Cell::Text(Cow::Borrowed(text)),
        Wire::Bytes(bytes) => Cell::Bytes(bytes),
        Wire::Int(n) => Cell::Number(Cow::Owned(n.to_string())),
        Wire::Float(x) if x.is_finite() => Cell::Number(Cow::Owned(float_text(*x))),
        Wire::Decimal(digits) => Cell::Number(Cow::Borrowed(digits)),
        Wire::Datetime { secs, nanos } => Cell::Text(Cow::Owned(
            rfc_3339(*secs, *nanos).unwrap_or_else(|| datetime_parts(*secs, *nanos)),
        )),
        Wire::Uuid(id) => Cell::Text(Cow::Owned(uuid_text(id))),
        other @ (Wire::Bool(_)
        | Wire::Float(_)
        | Wire::Duration { .. }
        | Wire::Record { .. }
        | Wire::Range(_)
        | Wire::Geometry { .. }
        | Wire::Set(_)
        | Wire::File { .. }
        | Wire::Array(_)
        | Wire::Object(_)
        | Wire::Unknown { .. }) => Cell::Text(Cow::Owned(surql_text(other))),
    }
}

/// `v` as compact SurrealQL, keeping `NONE` and `NULL` apart.
pub(crate) fn surql_text(v: &Wire) -> String {
    let mut out = String::new();
    write_value(&mut out, v);
    out
}

fn write_value(out: &mut String, v: &Wire) {
    match v {
        Wire::None => out.push_str("NONE"),
        Wire::Null => out.push_str("NULL"),
        Wire::Undefined => out.push_str("undefined"),
        Wire::Bool(b) => _ = write!(out, "{b}"),
        Wire::Int(n) => _ = write!(out, "{n}"),
        Wire::Float(x) => out.push_str(&float_text(*x)),
        Wire::Decimal(digits) => _ = write!(out, "{digits}dec"),
        Wire::Text(text) => out.push_str(&quote_string(text)),
        Wire::Bytes(bytes) => write_prefixed(out, 'b', &hex::encode(bytes)),
        // Past the formatter the parts stay bare, as `d'..'` would claim a literal they are not.
        Wire::Datetime { secs, nanos } => match rfc_3339(*secs, *nanos) {
            Some(text) => write_prefixed(out, 'd', &text),
            None => out.push_str(&datetime_parts(*secs, *nanos)),
        },
        Wire::Duration { secs, nanos } => out.push_str(&duration_text(*secs, *nanos)),
        Wire::Uuid(id) => write_prefixed(out, 'u', &uuid_text(id)),
        // A bare name here would read as a field.
        Wire::Table(name) => {
            out.push_str("<table>");
            out.push_str(&quote_string(name));
        }
        Wire::Record { table, key } => {
            write_name(out, table);
            out.push(':');
            write_key(out, key);
        }
        Wire::Range(ends) => write_range(out, &ends.0, &ends.1, write_value),
        Wire::Geometry { tag, items } => write_geometry(out, *tag, items),
        // In braces a leading `a:1` reads as an object's field, so that set is a cast instead.
        Wire::Set(items) if matches!(items.first(), Some(Wire::Record { .. })) => {
            out.push_str("<set>[");
            write_list(out, items, write_value);
            out.push(']');
        }
        // A one-item or empty set needs the comma, or it reads as a block or an object.
        Wire::Set(items) => {
            out.push('{');
            write_list(out, items, write_value);
            if items.len() < 2 {
                out.push(',');
            }
            out.push('}');
        }
        Wire::File { bucket, path } => write_prefixed(out, 'f', &format!("{bucket}:{path}")),
        Wire::Array(items) => {
            out.push('[');
            write_list(out, items, write_value);
            out.push(']');
        }
        Wire::Object(entries) => {
            out.push('{');
            write_list(out, entries, |out, (key, value)| {
                if is_identifier(key) {
                    out.push_str(key);
                } else {
                    out.push_str(&quote_string(key));
                }
                out.push_str(": ");
                write_value(out, value);
            });
            out.push('}');
        }
        Wire::Unknown { .. } => write_diagnostic(out, v),
    }
}

fn write_list<T>(out: &mut String, items: &[T], mut each: impl FnMut(&mut String, &T)) {
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        each(out, item);
    }
}

fn write_prefixed(out: &mut String, prefix: char, text: &str) {
    out.push(prefix);
    out.push_str(&quote_string(text));
}

// Angle quotes cannot write `⟩`, and they decode a backslash, so those names take backticks.
fn write_name(out: &mut String, name: &str) {
    if is_identifier(name) {
        out.push_str(name);
    } else if name.contains(['⟩', '\\']) {
        out.push_str(&quote_ident(name));
    } else {
        _ = write!(out, "⟨{name}⟩");
    }
}

// A record key takes names where a value takes strings, its range bounds included.
fn write_key(out: &mut String, key: &Wire) {
    match key {
        Wire::Text(name) => write_name(out, name),
        Wire::Range(ends) => write_range(out, &ends.0, &ends.1, write_key),
        other @ (Wire::None
        | Wire::Null
        | Wire::Undefined
        | Wire::Bool(_)
        | Wire::Int(_)
        | Wire::Float(_)
        | Wire::Decimal(_)
        | Wire::Bytes(_)
        | Wire::Datetime { .. }
        | Wire::Duration { .. }
        | Wire::Uuid(_)
        | Wire::Table(_)
        | Wire::Record { .. }
        | Wire::Geometry { .. }
        | Wire::Set(_)
        | Wire::File { .. }
        | Wire::Array(_)
        | Wire::Object(_)
        | Wire::Unknown { .. }) => write_value(out, other),
    }
}

fn write_range(
    out: &mut String,
    start: &Bound<Wire>,
    end: &Bound<Wire>,
    write_bound: fn(&mut String, &Wire),
) {
    match start {
        Bound::Included(v) => write_bound(out, v),
        Bound::Excluded(v) => {
            write_bound(out, v);
            out.push('>');
        }
        Bound::Unbounded => {}
    }
    out.push_str("..");
    match end {
        Bound::Included(v) => {
            out.push('=');
            write_bound(out, v);
        }
        Bound::Excluded(v) => write_bound(out, v),
        Bound::Unbounded => {}
    }
}

// A point has its own literal; every other geometry has only its GeoJSON object.
fn write_geometry(out: &mut String, tag: u64, items: &[Wire]) {
    let kind = match tag {
        88 => {
            out.push('(');
            write_list(out, items, write_value);
            out.push(')');
            return;
        }
        94 => {
            out.push_str("{type: 'GeometryCollection', geometries: [");
            write_list(out, items, write_value);
            out.push_str("]}");
            return;
        }
        89 => "LineString",
        90 => "Polygon",
        91 => "MultiPoint",
        92 => "MultiLineString",
        _ => "MultiPolygon",
    };
    _ = write!(out, "{{type: '{kind}', coordinates: ");
    write_coordinates(out, items);
    out.push('}');
}

// GeoJSON drops the tags: every nested geometry, points included, is a plain array.
fn write_coordinates(out: &mut String, items: &[Wire]) {
    out.push('[');
    write_list(out, items, |out, item| match item {
        Wire::Geometry { items, .. } => write_coordinates(out, items),
        other @ (Wire::None
        | Wire::Null
        | Wire::Undefined
        | Wire::Bool(_)
        | Wire::Int(_)
        | Wire::Float(_)
        | Wire::Decimal(_)
        | Wire::Text(_)
        | Wire::Bytes(_)
        | Wire::Datetime { .. }
        | Wire::Duration { .. }
        | Wire::Uuid(_)
        | Wire::Table(_)
        | Wire::Record { .. }
        | Wire::Range(_)
        | Wire::Set(_)
        | Wire::File { .. }
        | Wire::Array(_)
        | Wire::Object(_)
        | Wire::Unknown { .. }) => write_value(out, other),
    });
    out.push(']');
}

// CBOR diagnostic notation for what a tag this client does not know wraps. A known value inside
// keeps its SurrealQL text, where diagnostic notation would only spell its tag out again.
fn write_diagnostic(out: &mut String, v: &Wire) {
    match v {
        Wire::Unknown { tag, payload } => {
            _ = write!(out, "{tag}(");
            write_diagnostic(out, payload);
            out.push(')');
        }
        Wire::Null => out.push_str("null"),
        Wire::Text(text) => write_json_string(out, text),
        Wire::Bytes(bytes) => _ = write!(out, "h'{}'", hex::encode(bytes)),
        Wire::Array(items) => {
            out.push('[');
            write_list(out, items, write_diagnostic);
            out.push(']');
        }
        Wire::Object(entries) => {
            out.push('{');
            write_list(out, entries, |out, (key, value)| {
                write_json_string(out, key);
                out.push_str(": ");
                write_diagnostic(out, value);
            });
            out.push('}');
        }
        other @ (Wire::None
        | Wire::Undefined
        | Wire::Bool(_)
        | Wire::Int(_)
        | Wire::Float(_)
        | Wire::Decimal(_)
        | Wire::Datetime { .. }
        | Wire::Duration { .. }
        | Wire::Uuid(_)
        | Wire::Table(_)
        | Wire::Record { .. }
        | Wire::Range(_)
        | Wire::Geometry { .. }
        | Wire::Set(_)
        | Wire::File { .. }) => write_value(out, other),
    }
}

fn write_json_string(out: &mut String, text: &str) {
    _ = write!(out, "{}", serde_json::Value::from(text));
}

// Debug is the shortest text that round-trips and keeps `.0`, so a float never reads as an int.
fn float_text(x: f64) -> String {
    if x.is_infinite() {
        let sign = if x < 0.0 { "-" } else { "" };
        format!("{sign}Infinity")
    } else {
        format!("{x:?}")
    }
}

// `None` outside years 0000 to 9999, which RFC 3339 cannot write. Floored seconds with positive
// nanos are what the wire sends, so the Euclidean split is the time of day before 1970 too.
fn rfc_3339(secs: i64, nanos: u32) -> Option<String> {
    let (year, month, day) = civil(secs.div_euclid(SECONDS_PER_DAY));
    let of_day = secs.rem_euclid(SECONDS_PER_DAY);
    (0..=9999).contains(&year).then(|| {
        format!(
            "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}.{nanos:09}Z",
            of_day / 3_600,
            of_day / 60 % 60,
            of_day % 60,
        )
    })
}

fn datetime_parts(secs: i64, nanos: u32) -> String {
    format!("{secs}s {nanos}ns")
}

fn duration_text(secs: u64, nanos: u32) -> String {
    let mut left = u128::from(secs) * NANOS_PER_SECOND + u128::from(nanos);
    if left == 0 {
        return "0ns".to_owned();
    }
    let mut out = String::new();
    for (unit, name) in DURATION_UNITS {
        if left >= unit {
            _ = write!(out, "{}{name}", left / unit);
            left %= unit;
        }
    }
    out
}

fn uuid_text(id: &[u8; 16]) -> String {
    let digits = hex::encode(id);
    format!(
        "{}-{}-{}-{}-{}",
        &digits[..8],
        &digits[8..12],
        &digits[12..16],
        &digits[16..20],
        &digits[20..],
    )
}

#[cfg(test)]
mod tests;
