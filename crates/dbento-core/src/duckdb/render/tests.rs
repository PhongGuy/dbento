use ::duckdb::arrow::array::Array;
use ::duckdb::arrow::util::display::array_value_to_string;
use ::duckdb::{Config, Connection};

use super::Renderer;
use crate::value::CellRef;

#[derive(Debug, PartialEq)]
enum Cell {
    Null,
    Text(String),
    Bytes(Vec<u8>),
}

fn conn() -> Connection {
    let cfg = Config::default().enable_autoload_extension(false).unwrap();
    Connection::open_in_memory_with_flags(cfg).unwrap()
}

/// The renderer's verdict, DuckDB's cast text and DuckDB's `typeof()` for one expression.
type Seen = (Result<(String, Cell, bool), String>, Option<String>, String);

/// `expr` beside DuckDB's own spelling of it: the renderer's verdict (type text, cell,
/// number bit) or its undecodable type text, then `CAST(expr AS VARCHAR)` and `typeof(expr)`.
fn see(c: &Connection, expr: &str) -> Seen {
    let sql = format!("SELECT {expr} AS v, CAST({expr} AS VARCHAR) AS t, typeof({expr}) AS ty");
    let mut st = c.prepare(&sql).unwrap();
    drop(st.stream_arrow([]).unwrap());
    let schema = st.schema();
    let renderer = Renderer::for_column(&st.column_logical_type(0), schema.field(0).data_type());
    let chunk = st.step().unwrap().unwrap();
    let verdict = renderer.map_err(|u| u.type_text).map(|r| {
        let mut scratch = String::new();
        let cell = match r.render(chunk.column(0).as_ref(), 0, &mut scratch).unwrap() {
            CellRef::Null => Cell::Null,
            CellRef::Text(t) => Cell::Text(t.to_owned()),
            CellRef::Bytes(b) => Cell::Bytes(b.to_vec()),
        };
        (r.type_text().to_owned(), cell, r.is_number())
    });
    let cast = (!chunk.column(1).is_null(0))
        .then(|| array_value_to_string(chunk.column(1).as_ref(), 0).unwrap());
    let typeof_ = array_value_to_string(chunk.column(2).as_ref(), 0).unwrap();
    (verdict, cast, typeof_)
}

const NUMBERS: &[&str] = &[
    "(-128)::TINYINT",
    "32767::SMALLINT",
    "(-2147483648)::INTEGER",
    "9223372036854775807::BIGINT",
    "255::UTINYINT",
    "65535::USMALLINT",
    "4294967295::UINTEGER",
    "18446744073709551615::UBIGINT",
    "0.1::FLOAT",
    "(1/3)::FLOAT",
    "0.1::DOUBLE",
    "1.0::DOUBLE",
    "1e20::DOUBLE",
    "1e-7::DOUBLE",
    "123456789.125::DOUBLE",
    "(-0.0)::DOUBLE",
    "'nan'::DOUBLE",
    "'inf'::DOUBLE",
    "'-inf'::DOUBLE",
    "1.5::DECIMAL(4,1)",
    "(-0.05)::DECIMAL(4,2)",
    "123456789012345.678::DECIMAL(18,3)",
    "'12345678901234567890123456.7890123456'::DECIMAL(38,10)",
    "0::DECIMAL(38,2)",
    "170141183460469231731687303715884105727::HUGEINT",
    "(-170141183460469231731687303715884105728)::HUGEINT",
    "340282366920938463463374607431768211455::UHUGEINT",
    "0::UHUGEINT",
    "1e15::DOUBLE",
    "1e16::DOUBLE",
    "0.0001::DOUBLE",
    "0.00001::DOUBLE",
    "123456789012345678::DOUBLE",
    "1e15::FLOAT",
    "1e16::FLOAT",
    "0.00001::FLOAT",
];

const TEXTS: &[&str] = &[
    "true",
    "false",
    "'héllo'",
    "''",
    "'a''b'",
    "'6f1d7f7c-5b9b-4a3e-9c47-2d1f0b0c8a11'::UUID",
    "DATE '2024-02-29'",
    "DATE '0001-01-01'",
    "DATE '0044-03-15 (BC)'",
    "'infinity'::DATE",
    "'-infinity'::DATE",
    "TIME '12:34:56'",
    "TIME '00:00:00.5'",
    "TIME '23:59:59.999999'",
    "TIMESTAMP '2024-01-02 03:04:05'",
    "TIMESTAMP '2024-01-02 03:04:05.123456'",
    "'infinity'::TIMESTAMP",
    "'-infinity'::TIMESTAMP",
    "TIMESTAMP_S '2024-01-02 03:04:05'",
    "TIMESTAMP_MS '2024-01-02 03:04:05.123'",
    "TIMESTAMP_NS '2024-01-02 03:04:05.123456789'",
    "TIMESTAMPTZ '2024-01-02 03:04:05.5+00'",
    "'infinity'::TIMESTAMPTZ",
    "INTERVAL '1 year 2 months 3 days 04:05:06.5'",
    "INTERVAL '-1 day'",
    "INTERVAL '0 seconds'",
    "INTERVAL 36 HOURS",
    "TIMESTAMP '1969-12-31 23:59:59.5'",
    "TIMESTAMP '0044-03-15 (BC) 12:30:00'",
    "TIMESTAMP '294247-01-10 04:00:54.775806'",
    "TIMESTAMP_NS '1969-12-31 23:59:59.999999999'",
    "DATE '10000-01-01'",
    "INTERVAL '-1 year 2 days -00:00:01.5'",
    "INTERVAL 2000000 HOURS",
    "INTERVAL '1 microsecond'",
];

#[test]
fn every_scalar_matches_duckdbs_cast_and_typeof() {
    let c = conn();
    for (exprs, number) in [(NUMBERS, true), (TEXTS, false)] {
        for expr in exprs {
            let (verdict, cast, typeof_) = see(&c, expr);
            let (type_text, cell, is_number) =
                verdict.unwrap_or_else(|t| panic!("{expr}: undecodable {t}"));
            assert_eq!(cell, Cell::Text(cast.unwrap()), "{expr}");
            assert_eq!(type_text, typeof_, "{expr}");
            assert_eq!(is_number, number, "{expr}");
        }
    }
}

#[test]
fn the_oracle_itself_spells_the_spec_rows() {
    let c = conn();
    let text = |expr: &str| match see(&c, expr).0.unwrap().1 {
        Cell::Text(t) => t,
        other => panic!("{expr}: {other:?}"),
    };
    assert_eq!(
        text("170141183460469231731687303715884105727::HUGEINT"),
        "170141183460469231731687303715884105727"
    );
    assert_eq!(
        text("340282366920938463463374607431768211455::UHUGEINT"),
        "340282366920938463463374607431768211455"
    );
    assert_eq!(text("1.5::DECIMAL(4,1)"), "1.5");
    assert_eq!(text("'nan'::DOUBLE"), "nan");
    assert_eq!(text("'-inf'::DOUBLE"), "-inf");
    assert_eq!(text("'infinity'::DATE"), "infinity");
    assert_eq!(text("'-infinity'::TIMESTAMP"), "-infinity");
    assert!(text("TIMESTAMPTZ '2024-01-02 03:04:05+00'").ends_with("+00"));
}

#[test]
fn blobs_are_bytes_enums_text_and_nulls_null() {
    let c = conn();
    let (verdict, _, typeof_) = see(&c, "'\\xDE\\xAD'::BLOB");
    let (type_text, cell, number) = verdict.unwrap();
    assert_eq!(
        (type_text, cell, number),
        (typeof_, Cell::Bytes(vec![0xde, 0xad]), false)
    );
    let (verdict, _, _) = see(&c, "'b'::ENUM('a', 'b')");
    assert_eq!(
        verdict.unwrap(),
        ("ENUM".to_owned(), Cell::Text("b".to_owned()), false)
    );
    for expr in [
        "NULL::INTEGER",
        "NULL::VARCHAR",
        "NULL::BLOB",
        "NULL::DATE",
        "NULL::HUGEINT",
        "NULL::INTERVAL",
    ] {
        assert_eq!(see(&c, expr).0.unwrap().1, Cell::Null, "{expr}");
    }
}

const NESTED: &[&str] = &[
    "[1, 2]",
    "[]::INTEGER[]",
    "[NULL, 1]",
    "['a', 'b c', '', 'it''s', 'x,y', '[z]', 'NULL', 'null', ' lead', 'trail ', 'a\"b', '{k}', 'a=b', 'a:b', 'é']",
    "['  ', ' ', 'a ', 'Null', 'a\\b']",
    "[[1, 2], [3]]",
    "[1, 2, 3]::INTEGER[3]",
    "[[1, 2], [3, 4]]::INTEGER[2][2]",
    "[[1]]::INTEGER[][1]",
    "{'a': 1, 'b': 'x'}",
    "{'a': NULL::INTEGER, 'b': [1]}",
    "{'s': 'it''s', 't': 'a, b', 'u': ''}",
    "{'a': [1, 2]::INTEGER[2]}",
    "[{'a': 1}, NULL]",
    "row(1, 'a')",
    "MAP {'k1': 1, 'k2': 2}",
    "MAP {'a b': 'c, d', 'e': NULL}",
    "MAP {1: [1, 2]}",
    "MAP {}::MAP(VARCHAR, INTEGER)",
    "MAP {'k': {'x': 1}}",
    "union_value(num := 2)",
    "union_value(str := 'x y')",
    "[union_value(num := NULL::INTEGER)::UNION(num INTEGER, str VARCHAR), NULL]",
    "{'u': union_value(num := NULL::INTEGER)}",
    "[union_value(num := 2)]::UNION(num INTEGER, str VARCHAR)[]",
    "{'u': union_value(s := 'a,b')::UNION(n INTEGER, s VARCHAR)}",
    "[1.5::DECIMAL(4,1), NULL]",
    "[1.0::DOUBLE, 'nan'::DOUBLE, 1e20]",
    "[DATE '2024-01-01', 'infinity'::DATE, DATE '0044-03-15 (BC)']",
    "[TIME '12:00:00', NULL]",
    "[TIMESTAMPTZ '2024-01-02 03:04:05+00']",
    "[INTERVAL '1 day 1 second']",
    "['\\xDE\\xAD'::BLOB, 'a'::BLOB]",
    "{'b': '\\x00a'::BLOB}",
    "[true, false]",
    "[170141183460469231731687303715884105727::HUGEINT]",
    "[340282366920938463463374607431768211455::UHUGEINT]",
    "['6f1d7f7c-5b9b-4a3e-9c47-2d1f0b0c8a11'::UUID]",
];

#[test]
fn every_nested_value_matches_duckdbs_cast_and_typeof() {
    let c = conn();
    for expr in NESTED {
        let (verdict, cast, typeof_) = see(&c, expr);
        let (type_text, cell, number) =
            verdict.unwrap_or_else(|t| panic!("{expr}: undecodable {t}"));
        assert_eq!(cell, Cell::Text(cast.unwrap()), "{expr}");
        assert_eq!(type_text, typeof_, "{expr}");
        assert!(!number, "{expr}");
    }
}

#[test]
fn a_null_nested_value_is_a_null_cell() {
    let c = conn();
    for expr in [
        "NULL::INTEGER[]",
        "NULL::INTEGER[2]",
        "NULL::STRUCT(a INTEGER)",
        "NULL::MAP(VARCHAR, INTEGER)",
        "NULL::UNION(n INTEGER, s VARCHAR)",
    ] {
        assert_eq!(see(&c, expr).0.unwrap().1, Cell::Null, "{expr}");
    }
}

#[test]
fn a_union_whose_member_is_null_is_a_null_cell_at_the_top_level() {
    // Arrow cannot tell this from a NULL union.
    let (verdict, cast, _) = see(&conn(), "union_value(num := NULL::INTEGER)");
    assert_eq!(cast.as_deref(), Some("NULL"));
    assert_eq!(verdict.unwrap().1, Cell::Null);
}

#[test]
fn an_enum_inside_a_nested_value_is_spelled_enum() {
    let c = conn();
    for (expr, ty) in [
        ("['a'::ENUM('a', 'b')]", "ENUM[]"),
        ("{'e': 'b'::ENUM('a', 'b')}", "STRUCT(e ENUM)"),
    ] {
        let (verdict, cast, _) = see(&c, expr);
        let (type_text, cell, _) = verdict.unwrap();
        assert_eq!(
            (type_text.as_str(), cell),
            (ty, Cell::Text(cast.unwrap())),
            "{expr}"
        );
    }
}

const WIDE: &[&str] = &[
    "'101'::BIT",
    "'10110111001'::BIT",
    "'1'::BIT",
    "'00000000'::BIT",
    "'111111111'::BIT",
    "['101'::BIT, NULL]",
    "{'b': '0110'::BIT}",
    "0::BIGNUM",
    "1::BIGNUM",
    "(-1)::BIGNUM",
    "255::BIGNUM",
    "256::BIGNUM",
    "(-255)::BIGNUM",
    "(-256)::BIGNUM",
    "1180591620717411303424::BIGNUM",
    "'123456789012345678901234567890123456789012345678901234567890'::BIGNUM",
    "'-123456789012345678901234567890123456789012345678901234567890'::BIGNUM",
    "[1::BIGNUM, (-2)::BIGNUM]",
    "1::VARINT",
];

#[test]
fn bit_and_bignum_match_duckdbs_cast_and_typeof() {
    let c = conn();
    for expr in WIDE {
        let (verdict, cast, typeof_) = see(&c, expr);
        let (type_text, cell, number) =
            verdict.unwrap_or_else(|t| panic!("{expr}: undecodable {t}"));
        assert_eq!(cell, Cell::Text(cast.unwrap()), "{expr}");
        assert_eq!(type_text, typeof_, "{expr}");
        assert!(!number, "{expr}");
    }
}

#[test]
fn the_types_left_undecodable_name_themselves_as_typeof_does() {
    let c = conn();
    for expr in [
        "TIMETZ '12:00:00+02'",
        "[TIMETZ '12:00:00+02']",
        "{'t': TIMETZ '12:00:00+02'}",
        "MAP {'k': TIMETZ '12:00:00+02'}",
        "TIME_NS '12:00:00.123456789'",
    ] {
        let (verdict, _, typeof_) = see(&c, expr);
        assert_eq!(verdict.err(), Some(typeof_), "{expr}");
    }
}

#[test]
fn an_interval_past_the_arrow_appender_range_wraps() {
    // DuckDB's Arrow appender multiplies micros to nanos in int64 and wraps; a known limit.
    let (verdict, cast, _) = see(&conn(), "INTERVAL 3000000 HOURS");
    assert_eq!(
        verdict.unwrap().1,
        Cell::Text("-2124095:34:33.709551".to_owned())
    );
    assert_eq!(cast.as_deref(), Some("3000000:00:00"));
}

#[test]
fn member_names_are_quoted_as_typeof_quotes_them() {
    let c = conn();
    for expr in [
        "{'select': 1, 'A': 2, 'a b': 3, 'x\"y': 4, '1a': 5, 'order': 6, 'value': 7, 'name': 8, '_ok9': 9}",
        "'abc'::UNION(\"Num\" INTEGER, \"select\" VARCHAR)",
        "MAP {'k': {'Type': 1}}",
    ] {
        let (verdict, cast, typeof_) = see(&c, expr);
        let (type_text, cell, _) = verdict.unwrap();
        assert_eq!(type_text, typeof_, "{expr}");
        assert_eq!(cell, Cell::Text(cast.unwrap()), "{expr}");
    }
}

#[test]
fn the_keyword_list_is_duckdbs() {
    let c = conn();
    let mut st = c
        .prepare("SELECT keyword_name FROM duckdb_keywords()")
        .unwrap();
    let mut words: Vec<String> = st
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    words.sort();
    assert_eq!(super::KEYWORDS, words.as_slice());
}

#[test]
fn a_malformed_bit_or_bignum_buffer_is_refused_not_rendered() {
    use ::duckdb::arrow::array::BinaryArray;
    let cases: [(super::Kind, &str, &[&[u8]]); 2] = [
        (super::Kind::Bit, "BIT", &[b"", b"\x08\xff", b"\x05"]),
        (
            super::Kind::Bignum,
            "BIGNUM",
            &[
                b"",
                b"\x80\x00\x01",
                b"\x80\x00\x02\x05",
                b"\x80\x00\x01\x05\x06",
                b"\x7f\xff\xfe",
            ],
        ),
    ];
    for (kind, name, buffers) in cases {
        let r = Renderer {
            kind,
            type_text: name.to_owned(),
        };
        let column = BinaryArray::from_vec(buffers.to_vec());
        for (row, bytes) in buffers.iter().enumerate() {
            let mut scratch = String::new();
            assert!(
                r.render(&column, row, &mut scratch).is_err(),
                "{name} {bytes:02x?}"
            );
        }
    }
}
