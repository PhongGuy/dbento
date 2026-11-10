//! Cells from DuckDB Arrow chunks, keyed by the column's logical type.
//!
//! Arrow's own formatter is wrong at the HUGEINT extremes and fails on TIMESTAMPTZ and
//! infinities, so each scalar is formatted here from its typed array, spelled exactly as
//! DuckDB's `CAST(x AS VARCHAR)` spells it.

use std::fmt::Write as _;

use ::duckdb::arrow::array::{Array, AsArray, DictionaryArray};
use ::duckdb::arrow::datatypes::{
    ArrowDictionaryKeyType, DataType, Date32Type, Decimal128Type, Float32Type, Float64Type,
    Int8Type, Int16Type, Int32Type, Int64Type, IntervalMonthDayNanoType, IntervalUnit,
    Time64MicrosecondType, TimeUnit, TimestampMicrosecondType, TimestampMillisecondType,
    TimestampNanosecondType, TimestampSecondType, UInt8Type, UInt16Type, UInt32Type, UInt64Type,
    UnionMode,
};
use ::duckdb::core::{LogicalTypeHandle, LogicalTypeId};

use crate::numtext::{civil, decimal_text, float_text};
use crate::value::CellRef;

#[cfg(test)]
mod tests;

/// One column's decoder, chosen once from its logical and Arrow types.
#[derive(Debug)]
pub(crate) struct Renderer {
    kind: Kind,
    type_text: String,
}

/// A column whose type Dbento does not decode yet, named by its `typeof()` spelling.
#[derive(Debug)]
pub(crate) struct Undecodable {
    pub(crate) type_text: String,
}

/// A cell whose bytes do not follow its type's layout; the caller fails the statement.
#[derive(Debug)]
pub(crate) struct Malformed;

#[derive(Debug)]
enum Kind {
    Bool,
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
    F32,
    F64,
    Decimal(i8),
    Hugeint,
    Uhugeint,
    Text,
    Enum(EnumKey),
    Blob,
    Date,
    Time,
    Timestamp(TimeUnit, bool),
    Interval,
    List(Box<Renderer>),
    Array(Box<Renderer>),
    /// Members with their names; `true` when the struct is unnamed, as `row(...)` makes it.
    Struct(Vec<(String, Renderer)>, bool),
    Map(Box<Renderer>, Box<Renderer>),
    /// Members keyed by their Arrow type id.
    Union(Vec<(i8, Renderer)>),
    Bit,
    Bignum,
}

#[derive(Debug, Clone, Copy)]
enum EnumKey {
    U8,
    U16,
    U32,
}

impl Renderer {
    /// Picks the decoder for a column, or names its type when the logical type is not a
    /// plan-1 scalar or arrives as an Arrow type it is not expected to.
    pub(crate) fn for_column(
        logical: &LogicalTypeHandle,
        arrow: &DataType,
    ) -> Result<Renderer, Undecodable> {
        use LogicalTypeId as L;
        let kind = match (logical.id(), arrow) {
            (L::Boolean, DataType::Boolean) => Some(Kind::Bool),
            (L::Tinyint, DataType::Int8) => Some(Kind::I8),
            (L::Smallint, DataType::Int16) => Some(Kind::I16),
            (L::Integer, DataType::Int32) => Some(Kind::I32),
            (L::Bigint, DataType::Int64) => Some(Kind::I64),
            (L::UTinyint, DataType::UInt8) => Some(Kind::U8),
            (L::USmallint, DataType::UInt16) => Some(Kind::U16),
            (L::UInteger, DataType::UInt32) => Some(Kind::U32),
            (L::UBigint, DataType::UInt64) => Some(Kind::U64),
            (L::Float, DataType::Float32) => Some(Kind::F32),
            (L::Double, DataType::Float64) => Some(Kind::F64),
            (L::Decimal, DataType::Decimal128(_, scale)) => Some(Kind::Decimal(*scale)),
            (L::Hugeint, DataType::Decimal128(38, 0)) => Some(Kind::Hugeint),
            (L::UHugeint, DataType::Decimal128(38, 0)) => Some(Kind::Uhugeint),
            (L::Varchar | L::Uuid, DataType::Utf8) => Some(Kind::Text),
            #[allow(
                clippy::wildcard_enum_match_arm,
                reason = "DataType has too many variants to list; the rest share one arm"
            )]
            (L::Enum, DataType::Dictionary(key, value)) if **value == DataType::Utf8 => match **key
            {
                DataType::UInt8 => Some(Kind::Enum(EnumKey::U8)),
                DataType::UInt16 => Some(Kind::Enum(EnumKey::U16)),
                DataType::UInt32 => Some(Kind::Enum(EnumKey::U32)),
                _ => None,
            },
            (L::Blob, DataType::Binary) => Some(Kind::Blob),
            (L::Date, DataType::Date32) => Some(Kind::Date),
            (L::Time, DataType::Time64(TimeUnit::Microsecond)) => Some(Kind::Time),
            (L::Timestamp, DataType::Timestamp(TimeUnit::Microsecond, None)) => {
                Some(Kind::Timestamp(TimeUnit::Microsecond, false))
            }
            (L::TimestampS, DataType::Timestamp(TimeUnit::Second, None)) => {
                Some(Kind::Timestamp(TimeUnit::Second, false))
            }
            (L::TimestampMs, DataType::Timestamp(TimeUnit::Millisecond, None)) => {
                Some(Kind::Timestamp(TimeUnit::Millisecond, false))
            }
            (L::TimestampNs, DataType::Timestamp(TimeUnit::Nanosecond, None)) => {
                Some(Kind::Timestamp(TimeUnit::Nanosecond, false))
            }
            (L::TimestampTZ, DataType::Timestamp(TimeUnit::Microsecond, Some(_))) => {
                Some(Kind::Timestamp(TimeUnit::Microsecond, true))
            }
            (L::Interval, DataType::Interval(IntervalUnit::MonthDayNano)) => Some(Kind::Interval),
            (L::List | L::Array | L::Struct | L::Map | L::Union, _) => nested_kind(logical, arrow),
            (L::Bit, DataType::Binary) => Some(Kind::Bit),
            (L::Bignum, DataType::Binary) => Some(Kind::Bignum),
            _ => None,
        };
        let type_text = type_text(logical, Some(arrow));
        match kind {
            Some(kind) => Ok(Renderer { kind, type_text }),
            None => Err(Undecodable { type_text }),
        }
    }

    /// The column's type as DuckDB's `typeof()` spells it (`ENUM` for every enum).
    pub(crate) fn type_text(&self) -> &str {
        &self.type_text
    }

    /// Whether cells are numbers (integers, floats, decimals).
    pub(crate) fn is_number(&self) -> bool {
        matches!(
            self.kind,
            Kind::I8
                | Kind::I16
                | Kind::I32
                | Kind::I64
                | Kind::U8
                | Kind::U16
                | Kind::U32
                | Kind::U64
                | Kind::F32
                | Kind::F64
                | Kind::Decimal(_)
                | Kind::Hugeint
                | Kind::Uhugeint
        )
    }

    /// `row` of `column` (a chunk's Arrow column) as a cell; numbers render into `scratch`.
    /// Fails with `Malformed` when a BIT or BIGNUM buffer, at any depth, breaks its layout.
    pub(crate) fn render<'a>(
        &self,
        column: &'a dyn Array,
        row: usize,
        scratch: &'a mut String,
    ) -> Result<CellRef<'a>, Malformed> {
        if self.is_null(column, row) {
            return Ok(CellRef::Null);
        }
        scratch.clear();
        // Writing into a String cannot fail, so every `write!` result below is ignored.
        let s = &mut *scratch;
        match &self.kind {
            Kind::Bool => {
                return Ok(CellRef::Text(if column.as_boolean().value(row) {
                    "true"
                } else {
                    "false"
                }));
            }
            Kind::I8 => _ = write!(s, "{}", column.as_primitive::<Int8Type>().value(row)),
            Kind::I16 => _ = write!(s, "{}", column.as_primitive::<Int16Type>().value(row)),
            Kind::I32 => _ = write!(s, "{}", column.as_primitive::<Int32Type>().value(row)),
            Kind::I64 => _ = write!(s, "{}", column.as_primitive::<Int64Type>().value(row)),
            Kind::U8 => _ = write!(s, "{}", column.as_primitive::<UInt8Type>().value(row)),
            Kind::U16 => _ = write!(s, "{}", column.as_primitive::<UInt16Type>().value(row)),
            Kind::U32 => _ = write!(s, "{}", column.as_primitive::<UInt32Type>().value(row)),
            Kind::U64 => _ = write!(s, "{}", column.as_primitive::<UInt64Type>().value(row)),
            Kind::F32 => {
                let v = column.as_primitive::<Float32Type>().value(row);
                float_text(
                    s,
                    v.is_nan(),
                    v.is_infinite(),
                    v.is_sign_negative(),
                    format_args!("{v:e}"),
                );
            }
            Kind::F64 => {
                let v = column.as_primitive::<Float64Type>().value(row);
                float_text(
                    s,
                    v.is_nan(),
                    v.is_infinite(),
                    v.is_sign_negative(),
                    format_args!("{v:e}"),
                );
            }
            Kind::Decimal(scale) => {
                decimal_text(
                    s,
                    column.as_primitive::<Decimal128Type>().value(row),
                    *scale,
                );
            }
            Kind::Hugeint => {
                _ = write!(s, "{}", column.as_primitive::<Decimal128Type>().value(row))
            }
            Kind::Uhugeint => {
                let bits = column.as_primitive::<Decimal128Type>().value(row);
                _ = write!(s, "{}", bits.cast_unsigned());
            }
            Kind::Text => return Ok(CellRef::Text(column.as_string::<i32>().value(row))),
            Kind::Enum(EnumKey::U8) => {
                return Ok(CellRef::Text(enum_label::<UInt8Type>(column, row)));
            }
            Kind::Enum(EnumKey::U16) => {
                return Ok(CellRef::Text(enum_label::<UInt16Type>(column, row)));
            }
            Kind::Enum(EnumKey::U32) => {
                return Ok(CellRef::Text(enum_label::<UInt32Type>(column, row)));
            }
            Kind::Blob => return Ok(CellRef::Bytes(column.as_binary::<i32>().value(row))),
            Kind::Date => {
                let days = column.as_primitive::<Date32Type>().value(row);
                match days {
                    i32::MAX => s.push_str("infinity"),
                    d if d == -i32::MAX => s.push_str("-infinity"),
                    d => date(s, i64::from(d)),
                }
            }
            Kind::Time => {
                let micros = column.as_primitive::<Time64MicrosecondType>().value(row);
                time_of_day(s, micros * 1000, 6);
            }
            Kind::Timestamp(unit, tz) => {
                let (raw, per_second, digits) = match *unit {
                    TimeUnit::Second => (
                        column.as_primitive::<TimestampSecondType>().value(row),
                        1,
                        0,
                    ),
                    TimeUnit::Millisecond => (
                        column.as_primitive::<TimestampMillisecondType>().value(row),
                        1_000,
                        3,
                    ),
                    TimeUnit::Microsecond => (
                        column.as_primitive::<TimestampMicrosecondType>().value(row),
                        1_000_000,
                        6,
                    ),
                    TimeUnit::Nanosecond => (
                        column.as_primitive::<TimestampNanosecondType>().value(row),
                        1_000_000_000,
                        9,
                    ),
                };
                timestamp(s, raw, per_second, digits, *tz);
            }
            Kind::Interval => {
                let v = column.as_primitive::<IntervalMonthDayNanoType>().value(row);
                interval(s, v.months, v.days, v.nanoseconds / 1000);
            }
            Kind::List(_) | Kind::Array(_) | Kind::Struct(..) | Kind::Map(..) | Kind::Union(_) => {
                self.push_text(s, column, row, &mut String::new())?;
            }
            Kind::Bit => bit(s, column.as_binary::<i32>().value(row))?,
            Kind::Bignum => bignum(s, column.as_binary::<i32>().value(row))?,
        }
        Ok(CellRef::Text(scratch.as_str()))
    }

    /// Whether `row` is NULL; a union counts as NULL when its selected member is, since
    /// DuckDB's Arrow export writes both the same way.
    fn is_null(&self, column: &dyn Array, row: usize) -> bool {
        match &self.kind {
            Kind::Union(_) => {
                let u = column.as_union();
                let member = u.child(u.type_id(row));
                member.is_null(u.value_offset(row))
            }
            Kind::Bool
            | Kind::I8
            | Kind::I16
            | Kind::I32
            | Kind::I64
            | Kind::U8
            | Kind::U16
            | Kind::U32
            | Kind::U64
            | Kind::F32
            | Kind::F64
            | Kind::Decimal(_)
            | Kind::Hugeint
            | Kind::Uhugeint
            | Kind::Text
            | Kind::Enum(_)
            | Kind::Blob
            | Kind::Date
            | Kind::Time
            | Kind::Timestamp(..)
            | Kind::Interval
            | Kind::List(_)
            | Kind::Array(_)
            | Kind::Struct(..)
            | Kind::Map(..)
            | Kind::Bit
            | Kind::Bignum => column.is_null(row),
        }
    }

    fn is_nested(&self) -> bool {
        matches!(
            self.kind,
            Kind::List(_) | Kind::Array(_) | Kind::Struct(..) | Kind::Map(..) | Kind::Union(_)
        )
    }

    /// Appends `row` as a child of a nested value: `NULL`, a nested value raw, or a scalar's
    /// cast text through DuckDB's element-quoting rule. `tmp` is scratch for scalar text.
    fn push_child(
        &self,
        out: &mut String,
        column: &dyn Array,
        row: usize,
        tmp: &mut String,
    ) -> Result<(), Malformed> {
        if self.is_null(column, row) {
            out.push_str("NULL");
        } else if self.is_nested() {
            self.push_text(out, column, row, tmp)?;
        } else {
            match self.render(column, row, tmp)? {
                CellRef::Null => out.push_str("NULL"),
                CellRef::Text(t) => push_element(out, t, false),
                CellRef::Bytes(b) => {
                    let mut text = String::new();
                    blob_text(&mut text, b);
                    push_element(out, &text, false);
                }
            }
        }
        Ok(())
    }

    /// Appends the cast text of a non-NULL `row`: a scalar as `render` spells it (a BLOB as
    /// DuckDB's `\xHH` text), a nested value in DuckDB's VARCHAR-cast layout.
    fn push_text(
        &self,
        out: &mut String,
        column: &dyn Array,
        row: usize,
        tmp: &mut String,
    ) -> Result<(), Malformed> {
        let children = |out: &mut String,
                        child: &Renderer,
                        values: &dyn Array,
                        range: std::ops::Range<usize>,
                        tmp: &mut String| {
            out.push('[');
            for (n, i) in range.enumerate() {
                if n > 0 {
                    out.push_str(", ");
                }
                child.push_child(out, values, i, tmp)?;
            }
            out.push(']');
            Ok(())
        };
        match &self.kind {
            Kind::List(child) => {
                let list = column.as_list::<i32>();
                let o = list.value_offsets();
                let range = o[row] as usize..o[row + 1] as usize;
                children(out, child, list.values().as_ref(), range, tmp)?;
            }
            Kind::Array(child) => {
                let array = column.as_fixed_size_list();
                let start = array.value_offset_at(row);
                let range = start..start + array.value_length() as usize;
                children(out, child, array.values().as_ref(), range, tmp)?;
            }
            Kind::Struct(members, unnamed) => {
                let st = column.as_struct();
                out.push(if *unnamed { '(' } else { '{' });
                for (i, (name, member)) in members.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    if !*unnamed {
                        push_element(out, name, true);
                        out.push_str(": ");
                    }
                    member.push_child(out, st.column(i).as_ref(), row, tmp)?;
                }
                out.push(if *unnamed { ')' } else { '}' });
            }
            Kind::Map(key, value) => {
                let map = column.as_map();
                let o = map.value_offsets();
                out.push('{');
                for (n, i) in (o[row] as usize..o[row + 1] as usize).enumerate() {
                    if n > 0 {
                        out.push_str(", ");
                    }
                    key.push_child(out, map.keys().as_ref(), i, tmp)?;
                    out.push('=');
                    value.push_child(out, map.values().as_ref(), i, tmp)?;
                }
                out.push('}');
            }
            Kind::Union(members) => {
                // A union's text is its member's own text, unquoted (DuckDB counts it nested).
                let u = column.as_union();
                let id = u.type_id(row);
                let member = members.iter().find(|(t, _)| *t == id).map(|(_, r)| r);
                let child = u.child(id);
                let at = u.value_offset(row);
                match member {
                    // Callers saw the member non-NULL; every type id has a member.
                    Some(m) => m.push_text(out, child.as_ref(), at, tmp)?,
                    None => out.push_str("NULL"),
                }
            }
            Kind::Bool
            | Kind::I8
            | Kind::I16
            | Kind::I32
            | Kind::I64
            | Kind::U8
            | Kind::U16
            | Kind::U32
            | Kind::U64
            | Kind::F32
            | Kind::F64
            | Kind::Decimal(_)
            | Kind::Hugeint
            | Kind::Uhugeint
            | Kind::Text
            | Kind::Enum(_)
            | Kind::Blob
            | Kind::Date
            | Kind::Time
            | Kind::Timestamp(..)
            | Kind::Interval
            | Kind::Bit
            | Kind::Bignum => match self.render(column, row, tmp)? {
                CellRef::Null => out.push_str("NULL"),
                CellRef::Text(t) => out.push_str(t),
                CellRef::Bytes(b) => blob_text(out, b),
            },
        }
        Ok(())
    }
}

/// The nested kind for a LIST, ARRAY, STRUCT, MAP or UNION column, or `None` when its Arrow
/// shape is unexpected or any child is undecodable.
fn nested_kind(logical: &LogicalTypeHandle, arrow: &DataType) -> Option<Kind> {
    use LogicalTypeId as L;
    let child = |i: usize, arrow: &DataType| Renderer::for_column(&logical.child(i), arrow).ok();
    let n = logical.num_children();
    Some(match (logical.id(), arrow) {
        (L::List, DataType::List(f)) => Kind::List(Box::new(child(0, f.data_type())?)),
        (L::Array, DataType::FixedSizeList(f, _)) => {
            Kind::Array(Box::new(child(0, f.data_type())?))
        }
        (L::Struct, DataType::Struct(fields)) if fields.len() == n && n > 0 => {
            let members = (0..n)
                .map(|i| Some((logical.child_name(i), child(i, fields[i].data_type())?)))
                .collect::<Option<Vec<_>>>()?;
            let unnamed = members[0].0.is_empty();
            Kind::Struct(members, unnamed)
        }
        #[allow(
            clippy::wildcard_enum_match_arm,
            reason = "DataType has too many variants to list; the rest share one arm"
        )]
        (L::Map, DataType::Map(entries, _)) => match entries.data_type() {
            DataType::Struct(kv) if kv.len() == 2 => Kind::Map(
                Box::new(child(0, kv[0].data_type())?),
                Box::new(child(1, kv[1].data_type())?),
            ),
            _ => return None,
        },
        (L::Union, DataType::Union(fields, UnionMode::Sparse)) => {
            // Arrow's union members are not positional with DuckDB's, so match them by name.
            let members = fields
                .iter()
                .map(|(id, f)| {
                    let i = (0..n).find(|&i| logical.child_name(i) == *f.name())?;
                    Some((id, child(i, f.data_type())?))
                })
                .collect::<Option<Vec<_>>>()?;
            Kind::Union(members)
        }
        _ => return None,
    })
}

/// Appends `text` as DuckDB writes a scalar inside a nested value: quoted, with `'` and `\`
/// escaped, when it is empty, edged with a space, `null`, or holds a special byte; a struct
/// key (`key`) is always quoted (vector_cast_helpers.hpp:180-253).
fn push_element(out: &mut String, text: &str, key: bool) {
    const SPECIAL: &[u8] = b"\"'(),:=[\\]{}";
    let space = |c: u8| matches!(c, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r');
    let b = text.as_bytes();
    let quote = key
        || b.is_empty()
        || space(b[0])
        || (b.len() >= 2 && space(b[b.len() - 1]))
        || text.eq_ignore_ascii_case("null")
        || b.iter().any(|c| SPECIAL.contains(c));
    if !quote {
        out.push_str(text);
        return;
    }
    out.push('\'');
    for ch in text.chars() {
        if ch == '\'' || ch == '\\' {
            out.push('\\');
        }
        out.push(ch);
    }
    out.push('\'');
}

/// A BLOB as DuckDB's VARCHAR cast spells it: printable ASCII as-is, except `\`, `'` and
/// `"`, and every other byte as `\xHH` (blob.cpp:24-63).
fn blob_text(out: &mut String, bytes: &[u8]) {
    for &b in bytes {
        if (32..=126).contains(&b) && !matches!(b, b'\\' | b'\'' | b'"') {
            out.push(char::from(b));
        } else {
            _ = write!(out, "\\x{b:02X}");
        }
    }
}

/// DuckDB's BIT bytes as `0`/`1` text: byte 0 counts the padding bits that open byte 1,
/// and the bits run most significant first from there (bit.cpp:74-88).
fn bit(s: &mut String, bytes: &[u8]) -> Result<(), Malformed> {
    let Some((&padding, data)) = bytes.split_first() else {
        return Err(Malformed);
    };
    if data.is_empty() || padding > 7 {
        return Err(Malformed);
    }
    for (i, byte) in data.iter().enumerate() {
        let skip = if i == 0 { u32::from(padding) } else { 0 };
        for b in (0..8 - skip).rev() {
            s.push(if byte >> b & 1 == 1 { '1' } else { '0' });
        }
    }
    Ok(())
}

/// DuckDB's BIGNUM bytes in decimal: a 3-byte header holding the sign in its top bit and the
/// magnitude's length below it, inverted with every later byte when negative, then the
/// big-endian magnitude (bignum.cpp:58-70). A length that disagrees with the buffer fails.
fn bignum(s: &mut String, bytes: &[u8]) -> Result<(), Malformed> {
    const CHUNK: u128 = 10_000_000_000_000_000_000;
    let (&[h0, h1, h2], magnitude) = bytes.split_first_chunk::<3>().ok_or(Malformed)?;
    let negative = h0 & 0x80 == 0;
    let header = u32::from_be_bytes([0, h0, h1, h2]) ^ if negative { 0x00ff_ffff } else { 0 };
    if magnitude.is_empty() || (header & 0x007f_ffff) as usize != magnitude.len() {
        return Err(Malformed);
    }
    // Pack the magnitude into big-endian u64 limbs, then peel 19 decimal digits at a time
    // off the bottom by long division, so the chunks come out least significant first.
    let mut limbs: Vec<u64> = magnitude
        .rchunks(8)
        .rev()
        .map(|chunk| {
            chunk.iter().fold(0, |acc, &b| {
                acc << 8 | u64::from(if negative { !b } else { b })
            })
        })
        .collect();
    let mut chunks: Vec<u64> = Vec::new();
    while limbs.iter().any(|&l| l != 0) {
        let mut rem: u128 = 0;
        for limb in &mut limbs {
            let cur = rem << 64 | u128::from(*limb);
            *limb = (cur / CHUNK) as u64;
            rem = cur % CHUNK;
        }
        chunks.push(rem as u64);
    }
    let Some((top, rest)) = chunks.split_last() else {
        s.push('0');
        return Ok(());
    };
    if negative {
        s.push('-');
    }
    _ = write!(s, "{top}");
    for c in rest.iter().rev() {
        _ = write!(s, "{c:019}");
    }
    Ok(())
}

fn enum_label<K: ArrowDictionaryKeyType>(column: &dyn Array, row: usize) -> &str {
    let dict: &DictionaryArray<K> = column.as_dictionary();
    // The row is not null, so its key is present.
    let key = dict.key(row).unwrap_or_default();
    dict.values().as_string::<i32>().value(key)
}

/// A date as DuckDB prints it: `2024-02-29`, or `0044-03-15 (BC)` for 44 BC.
fn date(s: &mut String, days: i64) {
    let (year, month, day) = civil(days);
    let shown = if year <= 0 { 1 - year } else { year };
    _ = write!(s, "{shown:04}-{month:02}-{day:02}");
    if year <= 0 {
        s.push_str(" (BC)");
    }
}

/// `HH:MM:SS` plus the sub-second part to `digits` places, trailing zeros trimmed.
fn time_of_day(s: &mut String, nanos_of_day: i64, digits: u32) {
    let secs = nanos_of_day / 1_000_000_000;
    let frac = nanos_of_day % 1_000_000_000;
    _ = write!(
        s,
        "{:02}:{:02}:{:02}",
        secs / 3600,
        secs / 60 % 60,
        secs % 60
    );
    fraction(s, frac / 10i64.pow(9 - digits), digits);
}

fn fraction(s: &mut String, value: i64, digits: u32) {
    if value == 0 {
        return;
    }
    let mut text = format!("{value:0width$}", width = digits as usize);
    while text.ends_with('0') {
        text.pop();
    }
    s.push('.');
    s.push_str(&text);
}

/// A timestamp in `per_second` units since the epoch; DuckDB puts `(BC)` after the date
/// and `+00` after a TIMESTAMPTZ, which it holds in UTC.
fn timestamp(s: &mut String, raw: i64, per_second: i64, digits: u32, tz: bool) {
    if raw == i64::MAX {
        s.push_str("infinity");
        return;
    }
    if raw == -i64::MAX {
        s.push_str("-infinity");
        return;
    }
    let per_day = per_second * 86_400;
    let days = raw.div_euclid(per_day);
    let of_day = raw.rem_euclid(per_day);
    date(s, days);
    s.push(' ');
    let secs = of_day / per_second;
    _ = write!(
        s,
        "{:02}:{:02}:{:02}",
        secs / 3600,
        secs / 60 % 60,
        secs % 60
    );
    fraction(s, of_day % per_second, digits);
    if tz {
        s.push_str("+00");
    }
}

/// DuckDB's interval text: `1 year 2 months 3 days 04:05:06.5`, `-1 day`, `00:00:00`.
fn interval(s: &mut String, months: i32, days: i32, micros: i64) {
    let part = |s: &mut String, n: i32, unit: &str| {
        if n == 0 {
            return;
        }
        if !s.is_empty() {
            s.push(' ');
        }
        let plural = if n == 1 || n == -1 { "" } else { "s" };
        _ = write!(s, "{n} {unit}{plural}");
    };
    part(s, months / 12, "year");
    part(s, months % 12, "month");
    part(s, days, "day");
    if micros != 0 {
        if !s.is_empty() {
            s.push(' ');
        }
        if micros < 0 {
            s.push('-');
        }
        let m = micros.unsigned_abs();
        let secs = m / 1_000_000;
        _ = write!(
            s,
            "{:02}:{:02}:{:02}",
            secs / 3600,
            secs / 60 % 60,
            secs % 60
        );
        fraction(s, (m % 1_000_000) as i64, 6);
    } else if s.is_empty() {
        s.push_str("00:00:00");
    }
}

/// `typeof()`'s spelling of a logical type; `arrow` supplies the ARRAY sizes DuckDB's C
/// API does not expose, and is `None` below a level whose Arrow child is unknown.
fn type_text(logical: &LogicalTypeHandle, arrow: Option<&DataType>) -> String {
    use LogicalTypeId as L;
    let child_arrow = |i: usize| -> Option<&DataType> {
        #[allow(
            clippy::wildcard_enum_match_arm,
            reason = "DataType has too many variants to list; the rest share one arm"
        )]
        match arrow? {
            DataType::List(f) | DataType::LargeList(f) | DataType::FixedSizeList(f, _) => {
                Some(f.data_type())
            }
            DataType::Struct(fields) => fields.get(i).map(|f| f.data_type()),
            _ => None,
        }
    };
    // An unnamed struct (first member name empty, as `row(...)` makes) is DuckDB's TUPLE, which
    // lists bare types.
    let unnamed = || logical.num_children() > 0 && logical.child_name(0).is_empty();
    let fields = |out: &mut String| {
        let bare = unnamed();
        for i in 0..logical.num_children() {
            if i > 0 {
                out.push_str(", ");
            }
            if !bare {
                out.push_str(&quoted(&logical.child_name(i)));
                out.push(' ');
            }
            out.push_str(&type_text(&logical.child(i), child_arrow(i)));
        }
    };
    match logical.id() {
        L::Boolean => "BOOLEAN".into(),
        L::Tinyint => "TINYINT".into(),
        L::Smallint => "SMALLINT".into(),
        L::Integer => "INTEGER".into(),
        L::Bigint => "BIGINT".into(),
        L::UTinyint => "UTINYINT".into(),
        L::USmallint => "USMALLINT".into(),
        L::UInteger => "UINTEGER".into(),
        L::UBigint => "UBIGINT".into(),
        L::Float => "FLOAT".into(),
        L::Double => "DOUBLE".into(),
        L::Decimal => format!(
            "DECIMAL({},{})",
            logical.decimal_width(),
            logical.decimal_scale()
        ),
        L::Hugeint => "HUGEINT".into(),
        L::UHugeint => "UHUGEINT".into(),
        L::Varchar => "VARCHAR".into(),
        L::Uuid => "UUID".into(),
        L::Enum => "ENUM".into(),
        L::Blob => "BLOB".into(),
        L::Bit => "BIT".into(),
        L::Bignum => "BIGNUM".into(),
        L::Date => "DATE".into(),
        L::Time => "TIME".into(),
        L::TimeNs => "TIME_NS".into(),
        L::TimeTZ => "TIME WITH TIME ZONE".into(),
        L::Timestamp => "TIMESTAMP".into(),
        L::TimestampS => "TIMESTAMP_S".into(),
        L::TimestampMs => "TIMESTAMP_MS".into(),
        L::TimestampNs => "TIMESTAMP_NS".into(),
        L::TimestampTZ => "TIMESTAMP WITH TIME ZONE".into(),
        L::Interval => "INTERVAL".into(),
        L::List => format!("{}[]", type_text(&logical.child(0), child_arrow(0))),
        L::Array => {
            let size = match arrow {
                Some(DataType::FixedSizeList(_, n)) => n.to_string(),
                _ => "?".into(),
            };
            format!("{}[{size}]", type_text(&logical.child(0), child_arrow(0)))
        }
        L::Map => {
            // Arrow carries a map as a list of key/value structs; DuckDB names neither.
            let entry = match arrow {
                #[allow(
                    clippy::wildcard_enum_match_arm,
                    reason = "DataType has too many variants to list; the rest share one arm"
                )]
                Some(DataType::Map(f, _)) => match f.data_type() {
                    DataType::Struct(kv) if kv.len() == 2 => {
                        Some((kv[0].data_type().clone(), kv[1].data_type().clone()))
                    }
                    _ => None,
                },
                _ => None,
            };
            let (k, v) = match &entry {
                Some((k, v)) => (Some(k), Some(v)),
                None => (None, None),
            };
            format!(
                "MAP({}, {})",
                type_text(&logical.child(0), k),
                type_text(&logical.child(1), v)
            )
        }
        L::Struct => {
            let mut out = String::from(if unnamed() { "TUPLE(" } else { "STRUCT(" });
            fields(&mut out);
            out.push(')');
            out
        }
        L::Union => {
            // Arrow's union children are not positional with DuckDB's, so names come
            // from the logical type alone.
            let mut out = String::from("UNION(");
            for i in 0..logical.num_children() {
                if i > 0 {
                    out.push_str(", ");
                }
                out.push_str(&quoted(&logical.child_name(i)));
                out.push(' ');
                out.push_str(&type_text(&logical.child(i), None));
            }
            out.push(')');
            out
        }
        other @ (L::Invalid
        | L::Any
        | L::SqlNull
        | L::StringLiteral
        | L::IntegerLiteral
        | L::Geometry
        | L::Variant
        | L::Unsupported
        | _) => logical
            .get_alias()
            .unwrap_or_else(|| format!("{other:?}").to_uppercase()),
    }
}

/// A struct or union member name, double-quoted unless DuckDB's `typeof` leaves it bare: an
/// ASCII identifier of any case that does not start with a digit and is not a keyword.
fn quoted(name: &str) -> String {
    let plain = name
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && KEYWORDS
            .binary_search(&name.to_ascii_lowercase().as_str())
            .is_err();
    if plain {
        name.to_owned()
    } else {
        format!("\"{}\"", name.replace('"', "\"\""))
    }
}

/// Every DuckDB keyword (`duckdb_keywords()` on 2.0.0), lowercase and sorted; a member name
/// that is one is quoted in type text.
#[rustfmt::skip]
const KEYWORDS: &[&str] = &[
    "abort", "absolute", "access", "action", "add", "admin", "after", "aggregate", "all", "also",
    "alter", "always", "analyse", "analyze", "and", "anti", "any", "approx", "array", "as", "asc",
    "asof", "assertion", "assignment", "asymmetric", "at", "attach", "attribute", "authorization",
    "backward", "before", "begin", "between", "bigint", "binary", "bit", "boolean", "both", "by",
    "cache", "call", "called", "cascade", "cascaded", "case", "cast", "catalog", "centuries",
    "century", "chain", "char", "character", "characteristics", "check", "checkpoint", "class",
    "close", "cluster", "coalesce", "collate", "collation", "column", "columns", "columns",
    "comment", "comments", "commit", "committed", "compression", "concurrently", "configuration",
    "conflict", "connect", "connection", "constraint", "constraints", "content", "continue",
    "conversion", "copy", "cost", "create", "cross", "csv", "cube", "current", "cursor", "cycle",
    "data", "database", "day", "days", "deallocate", "dec", "decade", "decades", "decimal",
    "declare", "default", "defaults", "deferrable", "deferred", "definer", "delete", "delimiter",
    "delimiters", "depends", "desc", "describe", "destroy", "detach", "dictionary", "disable",
    "discard", "disconnect", "distance", "distinct", "do", "document", "domain", "double", "drop",
    "each", "else", "enable", "encoding", "encrypted", "end", "enum", "error", "escape", "event",
    "exact", "except", "exclude", "excluding", "exclusive", "execute", "exists", "explain",
    "export", "export_state", "extension", "extensions", "external", "extract", "false", "family",
    "fetch", "filter", "first", "float", "following", "for", "force", "foreign", "forward",
    "freeze", "from", "full", "function", "functions", "generated", "generated", "glob", "global",
    "grant", "granted", "group", "grouping", "grouping_id", "groups", "handler", "having", "header",
    "hold", "hour", "hours", "identity", "if", "ignore", "ilike", "immediate", "immutable",
    "implicit", "import", "in", "include", "including", "increment", "index", "indexes", "inherit",
    "inherits", "initially", "inline", "inner", "inout", "input", "insensitive", "insert",
    "install", "instead", "int", "integer", "intersect", "interval", "into", "invoker", "is",
    "isnull", "isolation", "join", "json", "key", "label", "lambda", "language", "large", "last",
    "lateral", "leading", "leakproof", "left", "level", "like", "limit", "listen", "load", "local",
    "location", "lock", "locked", "logged", "macro", "map", "map", "mapping", "match", "matched",
    "materialized", "maxvalue", "merge", "method", "microsecond", "microseconds", "millennia",
    "millennium", "millisecond", "milliseconds", "minute", "minutes", "minvalue", "mode", "month",
    "months", "move", "name", "names", "national", "natural", "nchar", "nearest", "new", "next",
    "no", "none", "not", "nothing", "notify", "notnull", "nowait", "null", "nullif", "nulls",
    "numeric", "object", "of", "off", "offset", "oids", "old", "on", "only", "operator", "option",
    "options", "or", "order", "ordinality", "others", "out", "outer", "over", "overlaps", "overlay",
    "overriding", "owned", "owner", "parallel", "parser", "partial", "partition", "partitioned",
    "passing", "password", "percent", "persistent", "pivot", "pivot_longer", "pivot_wider",
    "placing", "plans", "policy", "position", "positional", "pragma", "preceding", "precision",
    "prepare", "prepared", "preserve", "primary", "prior", "privileges", "procedural", "procedure",
    "program", "publication", "qualify", "quarter", "quarters", "quote", "range", "read", "real",
    "reassign", "recheck", "recursive", "ref", "references", "referencing", "refresh", "register",
    "reindex", "relative", "release", "rename", "repeatable", "replace", "replica", "reset",
    "resource", "resources", "respect", "restart", "restrict", "returning", "returns", "revoke",
    "right", "role", "rollback", "rollup", "row", "rows", "rule", "sample", "savepoint", "schema",
    "schemas", "scope", "scroll", "search", "second", "seconds", "secret", "secure", "security",
    "select", "semi", "sequence", "sequences", "serializable", "server", "session", "set", "setof",
    "sets", "share", "show", "similar", "similarity", "simple", "skip", "smallint", "snapshot",
    "some", "sorted", "source", "sql", "stable", "standalone", "start", "statement", "statistics",
    "stdin", "stdout", "storage", "stored", "strict", "strip", "struct", "struct", "subscription",
    "substring", "summarize", "symmetric", "sysid", "system", "table", "tables", "tablesample",
    "tablespace", "target", "temp", "template", "temporary", "text", "then", "ties", "time",
    "timestamp", "to", "trailing", "transaction", "transform", "treat", "trigger", "trim", "true",
    "truncate", "trusted", "try_cast", "try_cast", "tuple", "tuple", "type", "types", "unbounded",
    "uncommitted", "unencrypted", "union", "unique", "unknown", "unlisten", "unlogged", "unpack",
    "unpivot", "until", "update", "use", "user", "using", "vacuum", "valid", "validate",
    "validator", "value", "values", "varchar", "variable", "variadic", "varying", "verbose",
    "version", "view", "views", "virtual", "volatile", "week", "weeks", "when", "where",
    "whitespace", "window", "with", "within", "without", "work", "wrapper", "write", "xml",
    "xmlattributes", "xmlconcat", "xmlelement", "xmlexists", "xmlforest", "xmlnamespaces",
    "xmlparse", "xmlpi", "xmlroot", "xmlserialize", "xmltable", "year", "years", "yes", "zone",
];
