//! Decodes the server's CBOR replies into `Wire`, a value that keeps what SurrealDB tells apart:
//! null from NONE from undefined, and every tag. A frame comes from the network, so nesting,
//! declared lengths and the number of values are all bounded before anything is allocated.

use std::ops::Bound;

use cbor4ii::core::dec::{Decode, Read};
use cbor4ii::core::error::{DecodeError, Never};
use cbor4ii::core::utils::SliceReader;
use cbor4ii::core::{major, marker, types};

/// One decoded value. Object keys keep the wire's order, which the server sorts.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Wire {
    None,
    Null,
    Undefined,
    Bool(bool),
    Int(i128),
    Float(f64),
    Decimal(String),
    Text(String),
    Bytes(Vec<u8>),
    Datetime {
        secs: i64,
        nanos: u32,
    },
    Duration {
        secs: u64,
        nanos: u32,
    },
    Uuid([u8; 16]),
    Table(String),
    Record {
        table: String,
        key: Box<Wire>,
    },
    Range(Box<(Bound<Wire>, Bound<Wire>)>),
    /// Tags 88 to 94; `items` are floats for a point and nested geometries otherwise.
    Geometry {
        tag: u64,
        items: Vec<Wire>,
    },
    Set(Vec<Wire>),
    File {
        bucket: String,
        path: String,
    },
    Array(Vec<Wire>),
    Object(Vec<(String, Wire)>),
    /// A tag this client does not know, kept so a cell can show it.
    Unknown {
        tag: u64,
        payload: Box<Wire>,
    },
}

/// Why a reply could not be decoded.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub(crate) struct DecodeFailure {
    pub(crate) message: String,
}

impl From<DecodeError<Never>> for DecodeFailure {
    fn from(error: DecodeError<Never>) -> Self {
        failure(format!("malformed CBOR: {error}"))
    }
}

/// The values one frame may hold, so an array of one-byte items stays off the heap.
pub(crate) const MAX_VALUES: usize = 1 << 24;

/// Slots one container header may reserve: an exact fit for most, and nested headers cannot each
/// claim a frame's worth, because the value budget only charges items as they decode.
const MAX_RESERVED: usize = 1024;

const NANOS_PER_SECOND: u32 = 1_000_000_000;

/// Decodes one whole frame; bytes left after its value are a failure.
pub(crate) fn decode(frame: &[u8]) -> Result<Wire, DecodeFailure> {
    decode_bounded(frame, MAX_VALUES)
}

/// `decode` with its value budget as an argument, so the bound can be tested cheaply.
pub(crate) fn decode_bounded(frame: &[u8], max_values: usize) -> Result<Wire, DecodeFailure> {
    let mut reader = SliceReader::new(frame);
    let mut left = max_values;
    let out = value(&mut reader, &mut left)?;
    match remaining(&mut reader) {
        0 => Ok(out),
        extra => Err(failure(format!("{extra} bytes follow the reply's value"))),
    }
}

/// The top-level `id` of a frame that failed to decode, under any key order. The values of other
/// keys are stepped over by their heads alone, so one too deep or malformed to read is passed by.
pub(crate) fn envelope(frame: &[u8]) -> Option<i64> {
    let mut at = 0;
    let (major::MAP, entries) = cbor_head(frame, &mut at)? else {
        return None;
    };
    for _ in 0..entries {
        let (major::STRING, len) = cbor_head(frame, &mut at)? else {
            return None;
        };
        let end = at.checked_add(usize::try_from(len).ok()?)?;
        let key = frame.get(at..end)?;
        at = end;
        if key == b"id" {
            return i64::decode(&mut SliceReader::new(&frame[at..])).ok();
        }
        skip(frame, &mut at)?;
    }
    None
}

// One definite-length head: its major type and argument. Indefinite and reserved forms fail.
fn cbor_head(frame: &[u8], at: &mut usize) -> Option<(u8, u64)> {
    let first = *frame.get(*at)?;
    *at += 1;
    let size = match first & 0x1f {
        n @ 0..=23 => return Some((first >> 5, u64::from(n))),
        24 => 1,
        25 => 2,
        26 => 4,
        27 => 8,
        _ => return None,
    };
    let bytes = frame.get(*at..*at + size)?;
    *at += size;
    Some((
        first >> 5,
        bytes.iter().fold(0, |n, &b| n << 8 | u64::from(b)),
    ))
}

// Steps over one value without recursion: a count of items still owed replaces the call stack.
fn skip(frame: &[u8], at: &mut usize) -> Option<()> {
    let mut owed: u64 = 1;
    while owed > 0 {
        owed -= 1;
        let (kind, n) = cbor_head(frame, at)?;
        match kind {
            major::BYTES | major::STRING => {
                *at = at.checked_add(usize::try_from(n).ok()?)?;
                if *at > frame.len() {
                    return None;
                }
            }
            major::ARRAY => owed = owed.checked_add(n)?,
            major::MAP => owed = owed.checked_add(n.checked_mul(2)?)?,
            major::TAG => owed += 1,
            _ => {}
        }
    }
    Some(())
}

fn failure(message: impl Into<String>) -> DecodeFailure {
    DecodeFailure {
        message: message.into(),
    }
}

// The reader's own depth counter holds the nesting bound, so recursion cannot reach the stack.
fn value(r: &mut SliceReader<'_>, left: &mut usize) -> Result<Wire, DecodeFailure> {
    if !r.step_in() {
        return Err(failure("nesting exceeds the bound"));
    }
    let out = item(r, left);
    r.step_out();
    out
}

// Dispatch on the peeked first byte, because cbor4ii's `Value` and `Option` lose undefined.
fn item(r: &mut SliceReader<'_>, left: &mut usize) -> Result<Wire, DecodeFailure> {
    *left = left
        .checked_sub(1)
        .ok_or_else(|| failure("too many values in the reply"))?;
    let byte = peek(r)?;
    match byte >> 5 {
        major::UNSIGNED => Ok(Wire::Int(u64::decode(r)?.into())),
        major::NEGATIVE => {
            let types::Negative(n) = types::Negative::<u64>::decode(r)?;
            Ok(Wire::Int(-1 - i128::from(n)))
        }
        major::BYTES => {
            let types::Bytes(bytes) = types::Bytes::<&[u8]>::decode(r)?;
            Ok(Wire::Bytes(bytes.to_owned()))
        }
        major::STRING => Ok(Wire::Text(<&str>::decode(r)?.to_owned())),
        major::ARRAY => {
            let len = declared_length(types::Array::<()>::len(r)?, 1, r)?;
            let mut items = Vec::with_capacity(len.min(MAX_RESERVED));
            for _ in 0..len {
                items.push(value(r, left)?);
            }
            Ok(Wire::Array(items))
        }
        major::MAP => {
            let len = declared_length(types::Map::<()>::len(r)?, 2, r)?;
            let mut entries = Vec::with_capacity(len.min(MAX_RESERVED));
            for _ in 0..len {
                let key = <&str>::decode(r)?.to_owned();
                entries.push((key, value(r, left)?));
            }
            Ok(Wire::Object(entries))
        }
        major::TAG => {
            let tag = types::Tag::<()>::tag(r)?;
            let payload = value(r, left)?;
            tagged(tag, payload)
        }
        _ => simple(byte, r),
    }
}

fn simple(byte: u8, r: &mut SliceReader<'_>) -> Result<Wire, DecodeFailure> {
    match byte {
        marker::FALSE | marker::TRUE => Ok(Wire::Bool(bool::decode(r)?)),
        marker::NULL | marker::UNDEFINED => {
            r.advance(1);
            Ok(if byte == marker::NULL {
                Wire::Null
            } else {
                Wire::Undefined
            })
        }
        marker::F16 => Ok(Wire::Float(half::f16::decode(r)?.into())),
        marker::F32 => Ok(Wire::Float(f32::decode(r)?.into())),
        marker::F64 => Ok(Wire::Float(f64::decode(r)?)),
        other => Err(failure(format!("unsupported CBOR byte {other:#04x}"))),
    }
}

fn peek(r: &mut SliceReader<'_>) -> Result<u8, DecodeFailure> {
    match r.fill(1) {
        Ok(buf) => buf.as_ref().first().copied(),
        Err(never) => match never {},
    }
    .ok_or_else(|| failure("the reply ends early"))
}

fn remaining(r: &mut SliceReader<'_>) -> usize {
    match r.fill(usize::MAX) {
        Ok(buf) => buf.as_ref().len(),
        Err(never) => match never {},
    }
}

// `None` is an indefinite length, which the server never sends. A length needing more bytes than
// the frame has left is hostile or corrupt, whatever its items would have been.
fn declared_length(
    len: Option<usize>,
    bytes_per_item: usize,
    r: &mut SliceReader<'_>,
) -> Result<usize, DecodeFailure> {
    let len = len.ok_or_else(|| failure("an indefinite length is not accepted"))?;
    if len.saturating_mul(bytes_per_item) > remaining(r) {
        return Err(failure(format!(
            "declared length {len} is past the end of the reply"
        )));
    }
    Ok(len)
}

// Tag 6 is NONE whatever it carries. Tags 50 and 51 stay unknown here and tag 49 reads them, so a
// stray one elsewhere still shows as an unknown tag.
fn tagged(tag: u64, payload: Wire) -> Result<Wire, DecodeFailure> {
    match (tag, payload) {
        (6, _) => Ok(Wire::None),
        (7 | 8 | 10 | 12 | 14 | 37 | 49 | 55 | 56 | 88..=94, payload) => {
            shaped(tag, payload).ok_or_else(|| failure(format!("tag {tag} has the wrong payload")))
        }
        (tag, payload) => Ok(Wire::Unknown {
            tag,
            payload: Box::new(payload),
        }),
    }
}

// The value a known tag means, or `None` when its payload does not have that tag's shape.
fn shaped(tag: u64, payload: Wire) -> Option<Wire> {
    Some(match (tag, payload) {
        (7, Wire::Text(name)) => Wire::Table(name),
        (10, Wire::Text(digits)) => Wire::Decimal(digits),
        (37, Wire::Bytes(bytes)) => Wire::Uuid(bytes.try_into().ok()?),
        (56, Wire::Array(items)) => Wire::Set(items),
        (8, Wire::Array(parts)) => {
            let [Wire::Text(table), key] = two(parts)? else {
                return None;
            };
            Wire::Record {
                table,
                key: Box::new(key),
            }
        }
        (12, Wire::Array(parts)) => {
            let [Wire::Int(secs), nanos] = two(parts)? else {
                return None;
            };
            Wire::Datetime {
                secs: secs.try_into().ok()?,
                nanos: subsecond(&nanos)?,
            }
        }
        (14, Wire::Array(parts)) => {
            let (secs, nanos) = match parts.as_slice() {
                [] => (0, 0),
                [secs] => (seconds(secs)?, 0),
                [secs, nanos] => (seconds(secs)?, subsecond(nanos)?),
                _ => return None,
            };
            Wire::Duration { secs, nanos }
        }
        (49, Wire::Array(parts)) => {
            let [start, end] = two(parts)?;
            Wire::Range(Box::new((bound(start)?, bound(end)?)))
        }
        (55, Wire::Array(parts)) => {
            let [Wire::Text(bucket), Wire::Text(path)] = two(parts)? else {
                return None;
            };
            Wire::File { bucket, path }
        }
        (88, Wire::Array(items))
            if matches!(items.as_slice(), [Wire::Float(_), Wire::Float(_)]) =>
        {
            Wire::Geometry { tag, items }
        }
        (89..=94, Wire::Array(items)) => Wire::Geometry { tag, items },
        _ => return None,
    })
}

fn two(parts: Vec<Wire>) -> Option<[Wire; 2]> {
    parts.try_into().ok()
}

fn seconds(wire: &Wire) -> Option<u64> {
    match wire {
        Wire::Int(secs) => u64::try_from(*secs).ok(),
        _ => None,
    }
}

fn subsecond(wire: &Wire) -> Option<u32> {
    match wire {
        Wire::Int(nanos) => u32::try_from(*nanos)
            .ok()
            .filter(|nanos| *nanos < NANOS_PER_SECOND),
        _ => None,
    }
}

fn bound(end: Wire) -> Option<Bound<Wire>> {
    match end {
        Wire::Null => Some(Bound::Unbounded),
        Wire::Unknown { tag: 50, payload } => Some(Bound::Included(*payload)),
        Wire::Unknown { tag: 51, payload } => Some(Bound::Excluded(*payload)),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
