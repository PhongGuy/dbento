//! DynamoDB item values as typed JSON, grid cells, PartiQL literals and plain JSON.
//!
//! An SDK `AttributeValue` is turned into typed DynamoDB JSON once (`to_typed`), and every
//! other form is rendered from that, so no SDK type travels past this module.

use std::collections::{BTreeMap, HashMap};

use aws_sdk_dynamodb::types::AttributeValue;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use serde::de::IgnoredAny;
use serde_json::{Map, Value, json};

use crate::dynamo::lex::quote_string;

/// One rendered grid cell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Cell {
    Null,
    Text(String),
    Number(String),
    Bytes(Vec<u8>),
}

const UNKNOWN_LITERAL: &str = "(unknown type)";

/// One value as typed DynamoDB JSON: a one-key object such as {"S":"x"}; maps sorted by key,
/// B and BS as base64, an unknown type as {"$unknown":true}.
pub(crate) fn to_typed(v: &AttributeValue) -> Value {
    match v {
        AttributeValue::B(blob) => json!({ "B": STANDARD.encode(blob.as_ref()) }),
        AttributeValue::Bool(b) => json!({ "BOOL": b }),
        AttributeValue::Bs(blobs) => {
            let encoded: Vec<String> = blobs.iter().map(|b| STANDARD.encode(b.as_ref())).collect();
            json!({ "BS": encoded })
        }
        AttributeValue::L(items) => {
            json!({ "L": items.iter().map(to_typed).collect::<Vec<_>>() })
        }
        AttributeValue::M(map) => json!({ "M": Value::Object(item_to_typed(map)) }),
        AttributeValue::N(n) => json!({ "N": n }),
        AttributeValue::Ns(ns) => json!({ "NS": ns }),
        AttributeValue::Null(b) => json!({ "NULL": b }),
        AttributeValue::S(s) => json!({ "S": s }),
        AttributeValue::Ss(ss) => json!({ "SS": ss }),
        AttributeValue::Unknown { .. } => unknown(),
        _ => unknown(),
    }
}

/// An item as typed DynamoDB JSON with sorted keys.
pub(crate) fn item_to_typed(item: &HashMap<String, AttributeValue>) -> Map<String, Value> {
    // The map preserves insertion order, so inserting from a BTreeMap leaves the keys sorted.
    let sorted: BTreeMap<&String, &AttributeValue> = item.iter().collect();
    sorted
        .into_iter()
        .map(|(k, v)| (k.clone(), to_typed(v)))
        .collect()
}

fn unknown() -> Value {
    json!({ "$unknown": true })
}

/// The type tag and payload of a typed value, or `None` when it is not a one-key object.
fn tagged(typed: &Value) -> Option<(&str, &Value)> {
    match typed {
        Value::Object(map) if map.len() == 1 => map.iter().next().map(|(k, v)| (k.as_str(), v)),
        Value::Object(_)
        | Value::Null
        | Value::Bool(_)
        | Value::Number(_)
        | Value::String(_)
        | Value::Array(_) => None,
    }
}

/// A typed value split into what every renderer needs, with malformed payloads as `Unknown`.
enum Typed<'a> {
    S(&'a str),
    N(&'a str),
    B(&'a str),
    Bool(bool),
    Null,
    M(&'a Map<String, Value>),
    L(&'a [Value]),
    Ss(&'a [Value]),
    Ns(&'a [Value]),
    Bs(&'a [Value]),
    Unknown,
}

fn view(typed: &Value) -> Typed<'_> {
    let Some((tag, payload)) = tagged(typed) else {
        return Typed::Unknown;
    };
    match (tag, payload) {
        ("S", Value::String(s)) => Typed::S(s),
        ("N", Value::String(s)) => Typed::N(s),
        ("B", Value::String(s)) => Typed::B(s),
        ("BOOL", Value::Bool(b)) => Typed::Bool(*b),
        ("NULL", _) => Typed::Null,
        ("M", Value::Object(m)) => Typed::M(m),
        ("L", Value::Array(a)) => Typed::L(a),
        ("SS", Value::Array(a)) => Typed::Ss(a),
        ("NS", Value::Array(a)) => Typed::Ns(a),
        ("BS", Value::Array(a)) => Typed::Bs(a),
        _ => Typed::Unknown,
    }
}

/// The DynamoDB type name of a typed value: S N B BOOL NULL M L SS NS BS, else "unknown".
pub(crate) fn type_name(typed: &Value) -> &'static str {
    match view(typed) {
        Typed::S(_) => "S",
        Typed::N(_) => "N",
        Typed::B(_) => "B",
        Typed::Bool(_) => "BOOL",
        Typed::Null => "NULL",
        Typed::M(_) => "M",
        Typed::L(_) => "L",
        Typed::Ss(_) => "SS",
        Typed::Ns(_) => "NS",
        Typed::Bs(_) => "BS",
        Typed::Unknown => "unknown",
    }
}

/// A typed value as a grid cell: numbers keep their exact text, binary its bytes, and
/// collections show as their PartiQL literal.
pub(crate) fn cell(typed: &Value) -> Cell {
    match view(typed) {
        Typed::S(s) => Cell::Text(s.to_owned()),
        Typed::N(n) => Cell::Number(n.to_owned()),
        // Base64 that does not decode is shown as written rather than lost.
        Typed::B(b) => STANDARD
            .decode(b)
            .map_or_else(|_| Cell::Text(b.to_owned()), Cell::Bytes),
        Typed::Bool(b) => Cell::Text(b.to_string()),
        Typed::Null => Cell::Null,
        Typed::M(_) | Typed::L(_) | Typed::Ss(_) | Typed::Ns(_) | Typed::Bs(_) => {
            Cell::Text(literal(typed))
        }
        Typed::Unknown => Cell::Text(UNKNOWN_LITERAL.to_owned()),
    }
}

/// A typed value as a compact PartiQL literal.
pub(crate) fn literal(typed: &Value) -> String {
    let mut out = String::new();
    write_literal(&mut out, typed);
    out
}

/// An item as a PartiQL map literal: one line, or with `pretty` one top-level attribute per
/// line with nested values compact.
pub(crate) fn map_literal(item: &Map<String, Value>, pretty: bool) -> String {
    if item.is_empty() {
        return "{}".to_owned();
    }
    if !pretty {
        let mut out = String::new();
        write_map_literal(&mut out, item);
        return out;
    }
    let lines: Vec<String> = item
        .iter()
        .map(|(k, v)| format!("  {}: {}", quote_string(k), literal(v)))
        .collect();
    format!("{{\n{}\n}}", lines.join(",\n"))
}

fn write_literal(out: &mut String, typed: &Value) {
    match view(typed) {
        Typed::S(s) | Typed::B(s) => out.push_str(&quote_string(s)),
        Typed::N(n) => out.push_str(n),
        Typed::Bool(b) => out.push_str(if b { "true" } else { "false" }),
        Typed::Null => out.push_str("NULL"),
        Typed::M(m) => write_map_literal(out, m),
        Typed::L(items) => {
            out.push('[');
            write_list(out, items, write_literal);
            out.push(']');
        }
        Typed::Ss(items) | Typed::Bs(items) => {
            out.push_str("<<");
            write_list(out, items, |out, v| match v.as_str() {
                Some(s) => out.push_str(&quote_string(s)),
                None => out.push_str(UNKNOWN_LITERAL),
            });
            out.push_str(">>");
        }
        Typed::Ns(items) => {
            out.push_str("<<");
            write_list(out, items, |out, v| {
                out.push_str(v.as_str().unwrap_or(UNKNOWN_LITERAL));
            });
            out.push_str(">>");
        }
        Typed::Unknown => out.push_str(UNKNOWN_LITERAL),
    }
}

fn write_map_literal(out: &mut String, map: &Map<String, Value>) {
    out.push('{');
    for (i, (k, v)) in map.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        out.push_str(&quote_string(k));
        out.push_str(": ");
        write_literal(out, v);
    }
    out.push('}');
}

fn write_list(out: &mut String, items: &[Value], mut each: impl FnMut(&mut String, &Value)) {
    for (i, v) in items.iter().enumerate() {
        if i > 0 {
            out.push_str(", ");
        }
        each(out, v);
    }
}

/// A typed value as compact plain JSON, with numbers verbatim where JSON can spell them.
#[allow(
    dead_code,
    reason = "only the tests render a lone value; cells render whole items"
)]
pub(crate) fn plain_json(typed: &Value) -> String {
    let mut out = String::new();
    write_plain(&mut out, typed);
    out
}

/// An item as a compact plain JSON object.
pub(crate) fn map_plain_json(item: &Map<String, Value>) -> String {
    let mut out = String::new();
    write_plain_map(&mut out, item);
    out
}

/// A stored typed-JSON item as a PartiQL map literal, as `map_literal` renders it.
pub(crate) fn document_literal(bytes: &[u8], pretty: bool) -> String {
    decoded(bytes, |item| map_literal(item, pretty))
}

/// A stored typed-JSON item as compact plain JSON.
pub(crate) fn document_plain_json(bytes: &[u8]) -> String {
    decoded(bytes, map_plain_json)
}

// These return text with no error path, so a document that will not decode gives its reason.
fn decoded(bytes: &[u8], write: impl FnOnce(&Map<String, Value>) -> String) -> String {
    match serde_json::from_slice::<Value>(bytes) {
        Ok(Value::Object(item)) => write(&item),
        Ok(
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) | Value::Array(_),
        ) => "/* undecodable document: not a JSON object */".to_owned(),
        Err(e) => format!("/* undecodable document: {e} */"),
    }
}

// Written by hand because serde_json's Number cannot hold a 38-digit integer exactly.
fn write_plain(out: &mut String, typed: &Value) {
    match view(typed) {
        Typed::S(s) | Typed::B(s) => out.push_str(&json_string(s)),
        Typed::N(n) => write_plain_number(out, n),
        Typed::Bool(b) => out.push_str(if b { "true" } else { "false" }),
        Typed::Null => out.push_str("null"),
        Typed::M(m) => write_plain_map(out, m),
        Typed::L(items) => {
            out.push('[');
            write_plain_list(out, items, write_plain);
            out.push(']');
        }
        Typed::Ss(items) | Typed::Bs(items) => {
            out.push('[');
            write_plain_list(out, items, |out, v| match v.as_str() {
                Some(s) => out.push_str(&json_string(s)),
                None => out.push_str(&unknown().to_string()),
            });
            out.push(']');
        }
        Typed::Ns(items) => {
            out.push('[');
            write_plain_list(out, items, |out, v| match v.as_str() {
                Some(n) => write_plain_number(out, n),
                None => out.push_str(&unknown().to_string()),
            });
            out.push(']');
        }
        Typed::Unknown => out.push_str(&unknown().to_string()),
    }
}

fn write_plain_map(out: &mut String, map: &Map<String, Value>) {
    out.push('{');
    for (i, (k, v)) in map.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push_str(&json_string(k));
        out.push(':');
        write_plain(out, v);
    }
    out.push('}');
}

fn write_plain_list(out: &mut String, items: &[Value], mut each: impl FnMut(&mut String, &Value)) {
    for (i, v) in items.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        each(out, v);
    }
}

fn write_plain_number(out: &mut String, n: &str) {
    if is_json_number(n) {
        out.push_str(n);
    } else {
        out.push_str(&json_string(n));
    }
}

/// Whether `text` is a JSON number by grammar. The parse alone would also accept a quoted
/// string or surrounding whitespace, so the first and last characters must be number ones.
fn is_json_number(text: &str) -> bool {
    let starts = text.starts_with(|c: char| c == '-' || c.is_ascii_digit());
    let ends = text.ends_with(|c: char| c.is_ascii_digit());
    starts && ends && serde_json::from_str::<IgnoredAny>(text).is_ok()
}

fn json_string(s: &str) -> String {
    Value::String(s.to_owned()).to_string()
}

#[cfg(test)]
mod tests;
