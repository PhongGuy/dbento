//! BSON for reading: grid cells, mongosh syntax and Extended JSON. Every type renders as itself;
//! the deprecated ones fall back to canonical Extended JSON and none becomes null.

use std::ops::Range;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use bson::{Binary, Bson, DateTime, Document, JavaScriptCodeWithScope, RawDocument, Utf8Lossy};
use serde_json::{Value, json};

// Milliseconds RFC 3339 can spell, years 0 to 9999; bson clamps anything past them.
const SPELLABLE_DATES: Range<i64> = -62_167_219_200_000..253_402_300_800_000;

/// One grid cell: null, plain text, or digits with the number bit set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CellText {
    Null,
    Text(String),
    Number(String),
}

/// Decodes a stored document, invalid UTF-8 replaced, never failing on a string.
pub(crate) fn decode(raw: &RawDocument) -> Result<Document, String> {
    Utf8Lossy::<Document>::try_from(raw)
        .map(|lossy| lossy.0)
        .map_err(|e| e.to_string())
}

/// A grid cell, per the spec's cell list.
pub(crate) fn cell(value: &Bson) -> CellText {
    match value {
        Bson::Null => CellText::Null,
        Bson::String(s) => CellText::Text(s.clone()),
        Bson::Boolean(b) => CellText::Text(b.to_string()),
        Bson::Int32(n) => CellText::Number(n.to_string()),
        Bson::Int64(n) => CellText::Number(n.to_string()),
        Bson::Double(f) => CellText::Number(double_text(*f)),
        Bson::Decimal128(d) => CellText::Number(d.to_string()),
        other => CellText::Text(shell(other)),
    }
}

/// The `$type` alias of `value`.
pub(crate) fn type_name(value: &Bson) -> &'static str {
    match value {
        Bson::Double(_) => "double",
        Bson::String(_) => "string",
        Bson::Document(_) => "object",
        Bson::Array(_) => "array",
        Bson::Binary(_) => "binData",
        Bson::Undefined => "undefined",
        Bson::ObjectId(_) => "objectId",
        Bson::Boolean(_) => "bool",
        Bson::DateTime(_) => "date",
        Bson::Null => "null",
        Bson::RegularExpression(_) => "regex",
        Bson::DbPointer(_) => "dbPointer",
        Bson::JavaScriptCode(_) => "javascript",
        Bson::Symbol(_) => "symbol",
        Bson::JavaScriptCodeWithScope(_) => "javascriptWithScope",
        Bson::Int32(_) => "int",
        Bson::Timestamp(_) => "timestamp",
        Bson::Int64(_) => "long",
        Bson::Decimal128(_) => "decimal",
        Bson::MinKey => "minKey",
        Bson::MaxKey => "maxKey",
    }
}

/// Compact mongosh syntax.
pub(crate) fn shell(value: &Bson) -> String {
    let mut out = String::new();
    write_shell(&mut out, value);
    out
}

/// `shell(document)`, with every field and item on its own line, two spaces per level.
pub(crate) fn shell_pretty(document: &Document) -> String {
    let mut out = String::new();
    write_pretty_document(&mut out, document, 0);
    out
}

/// Extended JSON in field order, canonical or relaxed.
pub(crate) fn extended_json(document: Document, canonical: bool) -> String {
    let value = Bson::Document(document);
    let json = if canonical {
        value.into_canonical_extjson()
    } else {
        relaxed(value)
    };
    json.to_string()
}

// Relaxed Extended JSON by bson, except that a date RFC 3339 cannot spell takes its exact
// canonical form wherever it nests, because bson's relaxed output clamps it.
fn relaxed(value: Bson) -> Value {
    match value {
        Bson::Document(d) => Value::Object(d.into_iter().map(|(k, v)| (k, relaxed(v))).collect()),
        Bson::Array(items) => Value::Array(items.into_iter().map(relaxed).collect()),
        Bson::JavaScriptCodeWithScope(JavaScriptCodeWithScope { code, scope }) => {
            json!({ "$code": code, "$scope": relaxed(Bson::Document(scope)) })
        }
        Bson::DateTime(d) if !SPELLABLE_DATES.contains(&d.timestamp_millis()) => {
            value.into_canonical_extjson()
        }
        other => other.into_relaxed_extjson(),
    }
}

// JavaScript's number text: digits within its plain-decimal range, an exponent outside it with
// the sign mongosh prints, which Rust leaves off a positive one.
fn double_text(f: f64) -> String {
    if f.is_nan() {
        "NaN".to_owned()
    } else if f.is_infinite() {
        if f > 0.0 { "Infinity" } else { "-Infinity" }.to_owned()
    } else if f == 0.0 || (1e-6..1e21).contains(&f.abs()) {
        f.to_string()
    } else if f.abs() < 1.0 {
        format!("{f:e}")
    } else {
        format!("{f:e}").replacen('e', "e+", 1)
    }
}

fn write_shell(out: &mut String, value: &Bson) {
    match value {
        Bson::Document(d) => write_document(out, d),
        Bson::Array(items) => write_array(out, items),
        Bson::String(s) => write_quoted(out, s),
        Bson::Boolean(b) => out.push_str(&b.to_string()),
        Bson::Null => out.push_str("null"),
        Bson::Int32(n) => out.push_str(&n.to_string()),
        Bson::Double(f) => out.push_str(&double_text(*f)),
        Bson::Int64(n) => out.push_str(&format!("Long('{n}')")),
        Bson::Decimal128(d) => out.push_str(&format!("Decimal128('{d}')")),
        Bson::ObjectId(id) => out.push_str(&format!("ObjectId('{id}')")),
        Bson::DateTime(d) => out.push_str(&date_text(*d)),
        Bson::Binary(b) => out.push_str(&binary_text(b)),
        Bson::Timestamp(t) => {
            out.push_str(&format!(
                "Timestamp({{ t: {}, i: {} }})",
                t.time, t.increment
            ));
        }
        Bson::RegularExpression(re) => out.push_str(&re.to_string()),
        Bson::MinKey => out.push_str("MinKey()"),
        Bson::MaxKey => out.push_str("MaxKey()"),
        Bson::JavaScriptCode(code) => {
            out.push_str("Code(");
            write_quoted(out, code);
            out.push(')');
        }
        Bson::JavaScriptCodeWithScope(JavaScriptCodeWithScope { code, scope }) => {
            out.push_str("Code(");
            write_quoted(out, code);
            out.push_str(", ");
            write_document(out, scope);
            out.push(')');
        }
        Bson::Undefined | Bson::Symbol(_) | Bson::DbPointer(_) => {
            out.push_str(&value.clone().into_canonical_extjson().to_string());
        }
    }
}

fn write_document(out: &mut String, document: &Document) {
    if document.is_empty() {
        out.push_str("{}");
        return;
    }
    out.push_str("{ ");
    for (i, (key, value)) in document.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        write_key(out, key);
        write_shell(out, value);
    }
    out.push_str(" }");
}

fn write_array(out: &mut String, items: &[Bson]) {
    if items.is_empty() {
        out.push_str("[]");
        return;
    }
    out.push_str("[ ");
    for (i, item) in items.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        write_shell(out, item);
    }
    out.push_str(" ]");
}

fn write_pretty(out: &mut String, value: &Bson, depth: usize) {
    match value {
        Bson::Document(d) => write_pretty_document(out, d, depth),
        Bson::Array(items) if !items.is_empty() => {
            out.push_str("[\n");
            for (i, item) in items.iter().enumerate() {
                out.push_str(&"  ".repeat(depth + 1));
                write_pretty(out, item, depth + 1);
                end_line(out, i + 1 == items.len());
            }
            out.push_str(&"  ".repeat(depth));
            out.push(']');
        }
        other => write_shell(out, other),
    }
}

fn write_pretty_document(out: &mut String, document: &Document, depth: usize) {
    if document.is_empty() {
        out.push_str("{}");
        return;
    }
    out.push_str("{\n");
    for (i, (key, value)) in document.iter().enumerate() {
        out.push_str(&"  ".repeat(depth + 1));
        write_key(out, key);
        write_pretty(out, value, depth + 1);
        end_line(out, i + 1 == document.len());
    }
    out.push_str(&"  ".repeat(depth));
    out.push('}');
}

fn end_line(out: &mut String, last: bool) {
    out.push_str(if last { "\n" } else { ",\n" });
}

// A key stays bare only when it is a JavaScript identifier.
fn write_key(out: &mut String, key: &str) {
    let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_' || b == b'$';
    let bare = key.bytes().all(word) && !key.starts_with(|c: char| c.is_ascii_digit());
    if bare && !key.is_empty() {
        out.push_str(key);
    } else {
        write_quoted(out, key);
    }
    out.push_str(": ");
}

fn write_quoted(out: &mut String, s: &str) {
    out.push('\'');
    for c in s.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\'' => out.push_str("\\'"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c => out.push(c),
        }
    }
    out.push('\'');
}

// RFC 3339 trims the fraction, so the text is cut before it and the milliseconds always written.
fn date_text(date: DateTime) -> String {
    let ms = date.timestamp_millis();
    let spelled = SPELLABLE_DATES
        .contains(&ms)
        .then(|| date.try_to_rfc3339_string().ok())
        .flatten();
    match spelled.as_deref().and_then(|s| s.split(['.', 'Z']).next()) {
        Some(seconds) => format!("ISODate('{seconds}.{:03}Z')", ms.rem_euclid(1000)),
        None => format!("new Date({ms})"),
    }
}

// Only subtype 4 of 16 bytes is a UUID; anything else shows as the bytes it holds.
fn binary_text(binary: &Binary) -> String {
    match binary.to_uuid() {
        Ok(uuid) => format!("UUID('{uuid}')"),
        Err(_) => format!(
            "Binary.createFromBase64('{}', {})",
            STANDARD.encode(&binary.bytes),
            u8::from(binary.subtype)
        ),
    }
}

#[cfg(test)]
mod tests;
