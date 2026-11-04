//! SQL Server type names, spelled once for result columns and the catalog alike, and each
//! cell's text as the grid shows it.

use std::fmt::Write as _;

use tiberius::{ColumnData, FixedLenType as F, TypeInfo, VarLenType as V};

use crate::numtext::{civil, decimal_text, float_text};
use crate::session::ExecuteError;
use crate::value::CellRef;

/// The TDS user type of a `timestamp` (rowversion) column, sent as binary(8).
const ROWVERSION_USER_TYPE: u32 = 80;

/// The finest time, datetime2 and datetimeoffset scale: increments of 100 ns.
const MAX_TIME_SCALE: u8 = 7;

/// The largest decimal and numeric scale.
const MAX_DECIMAL_SCALE: i8 = 38;

/// Day numbers of SQL Server's two epochs in `civil`'s days since 1970-01-01.
const DAY_OF_0001: i64 = -719_162;
const DAY_OF_1900: i64 = -25_567;

/// A SQL Server type as the grid and the catalog spell it, from its `sys.types` base name
/// (lower case), its length in bytes (-1 for MAX), precision and scale.
pub(crate) fn type_text(base: &str, max_length: i32, precision: u8, scale: u8) -> String {
    let length = |unit: i32| match max_length {
        -1 => "MAX".to_owned(),
        n => (n / unit).to_string(),
    };
    let upper = base.to_ascii_uppercase();
    match base {
        "char" | "varchar" | "binary" | "varbinary" => format!("{upper}({})", length(1)),
        "nchar" | "nvarchar" => format!("{upper}({})", length(2)),
        "decimal" | "numeric" => format!("{upper}({precision},{scale})"),
        "time" | "datetime2" | "datetimeoffset" => format!("{upper}({scale})"),
        "timestamp" => "ROWVERSION".to_owned(),
        // A user CLR type arrives as `schema.name` and keeps its declared spelling.
        _ if base.contains('.') => base.to_owned(),
        _ => upper,
    }
}

/// A result column's type name and whether its cells are numbers, from its TDS type and
/// user type; `None` for a TDS type or width SQL Server has no spelling for.
pub(crate) fn wire_type(ty: &TypeInfo, user_type: u32) -> Option<(String, bool)> {
    // Map the wire type to its catalog base and sizes, so `type_text` spells both alike.
    let (base, max_length, precision, scale): (&str, i32, u8, u8) = match ty {
        TypeInfo::FixedLen(f) => match f {
            F::Int1 => ("tinyint", 0, 0, 0),
            F::Bit => ("bit", 0, 0, 0),
            F::Int2 => ("smallint", 0, 0, 0),
            F::Int4 => ("int", 0, 0, 0),
            F::Int8 => ("bigint", 0, 0, 0),
            F::Float4 => ("real", 0, 0, 0),
            F::Float8 => ("float", 0, 0, 0),
            F::Money => ("money", 0, 0, 0),
            F::Money4 => ("smallmoney", 0, 0, 0),
            F::Datetime => ("datetime", 0, 0, 0),
            F::Datetime4 => ("smalldatetime", 0, 0, 0),
            F::Null => return None,
        },
        TypeInfo::VarLenSized(cx) => {
            let len = cx.len();
            // 0xFFFF marks a MAX column on the three types that have one.
            let bytes = if len == 0xFFFF {
                -1
            } else {
                i32::try_from(len).ok()?
            };
            // A time scale above 7 is no SQL Server type, so it fails by name like an odd width.
            let scale = u8::try_from(len).ok().filter(|s| *s <= MAX_TIME_SCALE);
            match (cx.r#type(), len) {
                (V::Intn, 1) => ("tinyint", 0, 0, 0),
                (V::Intn, 2) => ("smallint", 0, 0, 0),
                (V::Intn, 4) => ("int", 0, 0, 0),
                (V::Intn, 8) => ("bigint", 0, 0, 0),
                (V::Bitn, _) => ("bit", 0, 0, 0),
                (V::Floatn, 4) => ("real", 0, 0, 0),
                (V::Floatn, 8) => ("float", 0, 0, 0),
                (V::Money, 4) => ("smallmoney", 0, 0, 0),
                (V::Money, 8) => ("money", 0, 0, 0),
                (V::Datetimen, 4) => ("smalldatetime", 0, 0, 0),
                (V::Datetimen, 8) => ("datetime", 0, 0, 0),
                (V::Daten, _) => ("date", 0, 0, 0),
                // The date and time types carry their scale where others carry a length.
                (V::Timen, _) => ("time", 0, 0, scale?),
                (V::Datetime2, _) => ("datetime2", 0, 0, scale?),
                (V::DatetimeOffsetn, _) => ("datetimeoffset", 0, 0, scale?),
                (V::Guid, _) => ("uniqueidentifier", 0, 0, 0),
                (V::BigVarChar, _) => ("varchar", bytes, 0, 0),
                (V::BigChar, _) => ("char", bytes, 0, 0),
                (V::NVarchar, _) => ("nvarchar", bytes, 0, 0),
                (V::NChar, _) => ("nchar", bytes, 0, 0),
                (V::BigVarBin, _) => ("varbinary", bytes, 0, 0),
                (V::BigBinary, _) if user_type == ROWVERSION_USER_TYPE => ("timestamp", 0, 0, 0),
                (V::BigBinary, _) => ("binary", bytes, 0, 0),
                (V::Text, _) => ("text", 0, 0, 0),
                (V::NText, _) => ("ntext", 0, 0, 0),
                (V::Image, _) => ("image", 0, 0, 0),
                (V::SSVariant, _) => ("sql_variant", 0, 0, 0),
                _ => return None,
            }
        }
        TypeInfo::VarLenSizedPrecision {
            ty,
            precision,
            scale,
            ..
        } => match ty {
            V::Decimaln => ("decimal", 0, *precision, *scale),
            V::Numericn => ("numeric", 0, *precision, *scale),
            _ => return None,
        },
        TypeInfo::Xml { .. } => ("xml", 0, 0, 0),
        // A system CLR type (hierarchyid, geography) shows its name; a user one its schema too.
        TypeInfo::Udt(udt) => {
            let name = if udt.schema_name == "sys" {
                udt.type_name.to_ascii_lowercase()
            } else {
                format!("{}.{}", udt.schema_name, udt.type_name)
            };
            return Some((type_text(&name, -1, 0, 0), false));
        }
    };
    let number = matches!(
        base,
        "bit"
            | "tinyint"
            | "smallint"
            | "int"
            | "bigint"
            | "real"
            | "float"
            | "decimal"
            | "numeric"
            | "money"
            | "smallmoney"
    );
    Some((type_text(base, max_length, precision, scale), number))
}

/// One cell as the grid shows it; a NULL of any type is `CellRef::Null`. Dates and times
/// read as the server's `CONVERT(nvarchar, x, 121)` prints them.
pub(crate) fn render<'a>(
    column: &str,
    data: &'a ColumnData<'static>,
    buf: &'a mut String,
) -> Result<CellRef<'a>, ExecuteError> {
    buf.clear();
    // Writing to a String cannot fail.
    let _ = match data {
        ColumnData::U8(Some(n)) => write!(buf, "{n}"),
        ColumnData::I16(Some(n)) => write!(buf, "{n}"),
        ColumnData::I32(Some(n)) => write!(buf, "{n}"),
        ColumnData::I64(Some(n)) => write!(buf, "{n}"),
        ColumnData::F32(Some(f)) => {
            let neg = f.is_sign_negative();
            float_text(buf, f.is_nan(), f.is_infinite(), neg, format_args!("{f:e}"));
            Ok(())
        }
        ColumnData::F64(Some(f)) => {
            let neg = f.is_sign_negative();
            float_text(buf, f.is_nan(), f.is_infinite(), neg, format_args!("{f:e}"));
            Ok(())
        }
        ColumnData::Bit(Some(b)) => return Ok(CellRef::Text(if *b { "1" } else { "0" })),
        ColumnData::String(Some(s)) => return Ok(CellRef::Text(s)),
        ColumnData::Xml(Some(x)) => return Ok(CellRef::Text(x.as_ref().as_ref())),
        ColumnData::Binary(Some(b)) => return Ok(CellRef::Bytes(b)),
        ColumnData::Guid(Some(g)) => write!(buf, "{g:X}"),
        ColumnData::Numeric(Some(n)) => {
            // Money arrives as a scale-4 Numeric, so it keeps its four digits too.
            decimal_text(buf, n.value(), decimal_scale(column, n.scale())?);
            Ok(())
        }
        ColumnData::Date(Some(d)) => {
            date(buf, DAY_OF_0001 + i64::from(d.days()));
            Ok(())
        }
        ColumnData::Time(Some(t)) => {
            let scale = scale_of(column, t.scale())?;
            clock(buf, t.increments(), scale);
            Ok(())
        }
        ColumnData::DateTime2(Some(dt)) => {
            let scale = scale_of(column, dt.time().scale())?;
            stamp(
                buf,
                i64::from(dt.date().days()),
                dt.time().increments(),
                scale,
                0,
            );
            Ok(())
        }
        ColumnData::DateTimeOffset(Some(dto)) => {
            // The wire value is UTC; the local stamp adds the offset, carrying the day.
            let dt = dto.datetime2();
            let scale = scale_of(column, dt.time().scale())?;
            let offset = i64::from(dto.offset());
            stamp(
                buf,
                i64::from(dt.date().days()),
                dt.time().increments(),
                scale,
                offset,
            );
            let sign = if offset < 0 { '-' } else { '+' };
            let m = offset.unsigned_abs();
            write!(buf, " {sign}{:02}:{:02}", m / 60, m % 60)
        }
        ColumnData::DateTime(Some(dt)) => {
            // 1/300 s ticks, rounded to the millisecond as the server prints them.
            let ms = (u64::from(dt.seconds_fragments()) * 10 + 1) / 3;
            date(buf, DAY_OF_1900 + i64::from(dt.days()));
            buf.push(' ');
            clock(buf, ms, 3);
            Ok(())
        }
        ColumnData::SmallDateTime(Some(dt)) => {
            let minutes = dt.seconds_fragments();
            date(buf, DAY_OF_1900 + i64::from(dt.days()));
            write!(buf, " {:02}:{:02}:00.000", minutes / 60, minutes % 60)
        }
        ColumnData::U8(None)
        | ColumnData::I16(None)
        | ColumnData::I32(None)
        | ColumnData::I64(None)
        | ColumnData::F32(None)
        | ColumnData::F64(None)
        | ColumnData::Bit(None)
        | ColumnData::String(None)
        | ColumnData::Xml(None)
        | ColumnData::Binary(None)
        | ColumnData::Guid(None)
        | ColumnData::Numeric(None)
        | ColumnData::Date(None)
        | ColumnData::Time(None)
        | ColumnData::DateTime2(None)
        | ColumnData::DateTimeOffset(None)
        | ColumnData::DateTime(None)
        | ColumnData::SmallDateTime(None) => return Ok(CellRef::Null),
    };
    Ok(CellRef::Text(buf))
}

/// A decimal scale `decimal_text` can print; tiberius's own constructor refuses more
/// than 38, so the check stands apart to be testable.
fn decimal_scale(column: &str, scale: u8) -> Result<i8, ExecuteError> {
    match i8::try_from(scale) {
        Ok(s) if s <= MAX_DECIMAL_SCALE => Ok(s),
        _ => Err(odd_scale(column, "decimal", scale)),
    }
}

/// A time scale the clock can print, as a power of ten.
fn scale_of(column: &str, scale: u8) -> Result<u32, ExecuteError> {
    if scale > MAX_TIME_SCALE {
        return Err(odd_scale(column, "time", scale));
    }
    Ok(u32::from(scale))
}

/// A value whose scale no SQL Server type has; the item fails by name, never as NULL.
fn odd_scale(column: &str, what: &str, scale: u8) -> ExecuteError {
    ExecuteError::Unsupported {
        reason: format!(
            "column {column} sent a {what} of scale {scale}, which no SQL Server type has"
        ),
    }
}

/// `yyyy-mm-dd` from days since 1970-01-01.
fn date(buf: &mut String, days: i64) {
    let (y, m, d) = civil(days);
    let _ = write!(buf, "{y:04}-{m:02}-{d:02}");
}

/// `hh:mi:ss` from increments of 10^-scale s since midnight, then `scale` fraction digits.
fn clock(buf: &mut String, increments: u64, scale: u32) {
    let per_second = 10u64.pow(scale);
    let secs = increments / per_second;
    let _ = write!(
        buf,
        "{:02}:{:02}:{:02}",
        secs / 3600,
        secs / 60 % 60,
        secs % 60
    );
    if scale > 0 {
        let _ = write!(buf, ".{:0w$}", increments % per_second, w = scale as usize);
    }
}

/// A date and time from days since 0001-01-01 and the time's increments, shifted by
/// `offset` minutes with the day carried either way.
fn stamp(buf: &mut String, days: i64, increments: u64, scale: u32, offset: i64) {
    let per_second = 10i64.pow(scale);
    let per_day = 86_400 * per_second;
    let total = days * per_day + increments as i64 + offset * 60 * per_second;
    date(buf, DAY_OF_0001 + total.div_euclid(per_day));
    buf.push(' ');
    clock(buf, total.rem_euclid(per_day) as u64, scale);
}

#[cfg(test)]
mod tests {
    use super::*;
    use tiberius::{
        Collation, FixedLenType as F, TypeInfo, UdtInfo, VarLenContext, VarLenType as V,
    };

    #[test]
    fn type_text_spells_lengths_precision_and_scale_as_declared() {
        let cases = [
            (("nvarchar", 20, 0, 0), "NVARCHAR(10)"),
            (("nvarchar", -1, 0, 0), "NVARCHAR(MAX)"),
            (("varchar", 8000, 0, 0), "VARCHAR(8000)"),
            (("varbinary", -1, 0, 0), "VARBINARY(MAX)"),
            (("char", 3, 0, 0), "CHAR(3)"),
            (("nchar", 8, 0, 0), "NCHAR(4)"),
            (("binary", 16, 0, 0), "BINARY(16)"),
            (("decimal", 9, 10, 2), "DECIMAL(10,2)"),
            (("numeric", 17, 38, 0), "NUMERIC(38,0)"),
            (("datetime2", 7, 23, 3), "DATETIME2(3)"),
            (("time", 3, 8, 0), "TIME(0)"),
            (("datetimeoffset", 10, 34, 7), "DATETIMEOFFSET(7)"),
            (("timestamp", 8, 0, 0), "ROWVERSION"),
            (("int", 4, 10, 0), "INT"),
            (("real", 4, 24, 0), "REAL"),
            (("float", 8, 53, 0), "FLOAT"),
            (("money", 8, 19, 4), "MONEY"),
            (("datetime", 8, 23, 3), "DATETIME"),
            (("xml", -1, 0, 0), "XML"),
            (("sql_variant", 8016, 0, 0), "SQL_VARIANT"),
            (("uniqueidentifier", 16, 0, 0), "UNIQUEIDENTIFIER"),
            (("hierarchyid", 892, 0, 0), "HIERARCHYID"),
            (("text", 16, 0, 0), "TEXT"),
            (("app.Point", -1, 0, 0), "app.Point"),
        ];
        for ((base, len, p, s), want) in cases {
            assert_eq!(type_text(base, len, p, s), want, "{base}");
        }
    }

    fn sized(ty: V, len: usize) -> TypeInfo {
        let collation = matches!(ty, V::BigChar | V::BigVarChar | V::NChar | V::NVarchar)
            .then(|| Collation::new(0x0409, 52));
        TypeInfo::VarLenSized(VarLenContext::new(ty, len, collation))
    }

    type Named = Option<(String, bool)>;

    fn name(ty: &TypeInfo, user_type: u32) -> Named {
        wire_type(ty, user_type)
    }

    #[test]
    fn wire_types_name_every_tds_type_with_its_size_and_number_bit() {
        let n = |s: &str| Some((s.to_owned(), true));
        let t = |s: &str| Some((s.to_owned(), false));
        let cases: Vec<(TypeInfo, u32, Named)> = vec![
            (TypeInfo::FixedLen(F::Int1), 0, n("TINYINT")),
            (TypeInfo::FixedLen(F::Bit), 0, n("BIT")),
            (TypeInfo::FixedLen(F::Int8), 0, n("BIGINT")),
            (TypeInfo::FixedLen(F::Float4), 0, n("REAL")),
            (TypeInfo::FixedLen(F::Float8), 0, n("FLOAT")),
            (TypeInfo::FixedLen(F::Money), 0, n("MONEY")),
            (TypeInfo::FixedLen(F::Money4), 0, n("SMALLMONEY")),
            (TypeInfo::FixedLen(F::Datetime), 0, t("DATETIME")),
            (TypeInfo::FixedLen(F::Datetime4), 0, t("SMALLDATETIME")),
            (sized(V::Intn, 2), 0, n("SMALLINT")),
            (sized(V::Intn, 4), 0, n("INT")),
            (sized(V::Bitn, 1), 0, n("BIT")),
            (sized(V::Floatn, 4), 0, n("REAL")),
            (sized(V::Floatn, 8), 0, n("FLOAT")),
            (sized(V::Money, 4), 0, n("SMALLMONEY")),
            (sized(V::Datetimen, 4), 0, t("SMALLDATETIME")),
            (sized(V::Datetimen, 8), 0, t("DATETIME")),
            (sized(V::Daten, 3), 0, t("DATE")),
            (sized(V::Timen, 7), 0, t("TIME(7)")),
            (sized(V::Datetime2, 0), 0, t("DATETIME2(0)")),
            (sized(V::DatetimeOffsetn, 7), 0, t("DATETIMEOFFSET(7)")),
            (sized(V::Guid, 16), 0, t("UNIQUEIDENTIFIER")),
            (sized(V::BigVarChar, 20), 0, t("VARCHAR(20)")),
            (sized(V::BigVarChar, 0xFFFF), 0, t("VARCHAR(MAX)")),
            (sized(V::BigChar, 3), 0, t("CHAR(3)")),
            (sized(V::NVarchar, 40), 0, t("NVARCHAR(20)")),
            (sized(V::NVarchar, 0xFFFF), 0, t("NVARCHAR(MAX)")),
            (sized(V::NChar, 8), 0, t("NCHAR(4)")),
            (sized(V::BigVarBin, 10), 0, t("VARBINARY(10)")),
            (sized(V::BigVarBin, 0xFFFF), 0, t("VARBINARY(MAX)")),
            (sized(V::BigBinary, 8), 0, t("BINARY(8)")),
            (sized(V::BigBinary, 8), 80, t("ROWVERSION")),
            (sized(V::Text, 2_147_483_647), 0, t("TEXT")),
            (sized(V::NText, 2_147_483_646), 0, t("NTEXT")),
            (sized(V::Image, 2_147_483_647), 0, t("IMAGE")),
            (sized(V::SSVariant, 8016), 0, t("SQL_VARIANT")),
            (
                TypeInfo::Xml {
                    schema: None,
                    size: 0xFFFF,
                },
                0,
                t("XML"),
            ),
            (
                TypeInfo::VarLenSizedPrecision {
                    ty: V::Decimaln,
                    size: 9,
                    precision: 10,
                    scale: 2,
                },
                0,
                n("DECIMAL(10,2)"),
            ),
            (
                TypeInfo::VarLenSizedPrecision {
                    ty: V::Numericn,
                    size: 17,
                    precision: 38,
                    scale: 0,
                },
                0,
                n("NUMERIC(38,0)"),
            ),
        ];
        for (ty, user, want) in cases {
            assert_eq!(name(&ty, user), want, "{ty:?}");
        }
    }

    #[test]
    fn clr_types_show_their_name_and_odd_widths_have_none() {
        let udt = |schema: &str, ty: &str| {
            TypeInfo::Udt(UdtInfo {
                max_byte_size: 892,
                db_name: "d".into(),
                schema_name: schema.into(),
                type_name: ty.into(),
                assembly_qualified_name: "a".into(),
            })
        };
        assert_eq!(
            name(&udt("sys", "hierarchyid"), 0),
            Some(("HIERARCHYID".into(), false))
        );
        assert_eq!(
            name(&udt("sys", "geography"), 0),
            Some(("GEOGRAPHY".into(), false))
        );
        assert_eq!(
            name(&udt("app", "Point"), 0),
            Some(("app.Point".into(), false))
        );
        for (ty, len) in [
            (V::Floatn, 6),
            (V::Intn, 3),
            (V::Money, 6),
            (V::Datetimen, 2),
        ] {
            assert_eq!(name(&sized(ty, len), 0), None, "{ty:?} {len}");
        }
        assert_eq!(name(&TypeInfo::FixedLen(F::Null), 0), None);
        assert_eq!(name(&sized(V::Timen, 8), 0), None);
    }

    fn cell(data: &ColumnData<'static>) -> String {
        let mut buf = String::new();
        match render("c", data, &mut buf) {
            Ok(CellRef::Text(t)) => t.to_owned(),
            other => panic!("not text: {other:?}"),
        }
    }

    /// A datetimeoffset(7) from days since 0001-01-01, UTC seconds and offset minutes.
    fn dto(days: u32, secs: u64, offset: i16) -> ColumnData<'static> {
        let time = tiberius::time::Time::new(secs * 10_000_000, 7);
        let dt2 = tiberius::time::DateTime2::new(tiberius::time::Date::new(days), time);
        ColumnData::DateTimeOffset(Some(tiberius::time::DateTimeOffset::new(dt2, offset)))
    }

    #[test]
    fn datetimeoffset_carries_the_day_across_midnight_both_ways() {
        // UTC 0001-01-02 05:00 at -14:00 falls back into the previous day.
        assert_eq!(
            cell(&dto(1, 5 * 3600, -840)),
            "0001-01-01 15:00:00.0000000 -14:00"
        );
        // UTC 2024-02-28 20:00 at +05:30 moves forward into the leap day.
        assert_eq!(
            cell(&dto(738_943, 20 * 3600, 330)),
            "2024-02-29 01:30:00.0000000 +05:30"
        );
        // The type's extremes, stored as UTC 14:00 and 09:59:59.
        assert_eq!(
            cell(&dto(0, 14 * 3600, -840)),
            "0001-01-01 00:00:00.0000000 -14:00"
        );
        assert_eq!(
            cell(&dto(3_652_058, 9 * 3600 + 59 * 60 + 59, 840)),
            "9999-12-31 23:59:59.0000000 +14:00"
        );
    }

    #[test]
    fn scales_no_sql_server_type_has_fail_by_name() {
        let mut buf = String::new();
        let time = ColumnData::Time(Some(tiberius::time::Time::new(0, 8)));
        let err = render("t", &time, &mut buf).unwrap_err();
        assert!(
            err.to_string().contains("column t sent a time of scale 8"),
            "{err}"
        );
        assert_eq!(decimal_scale("d", 38).ok(), Some(38));
        let err = decimal_scale("d", 39).unwrap_err();
        assert!(
            err.to_string()
                .contains("column d sent a decimal of scale 39"),
            "{err}"
        );
    }
}
