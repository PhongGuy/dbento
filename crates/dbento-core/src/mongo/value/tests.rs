use super::{Cursor, NOT_JS, ParseError, value};
use crate::mongo::lex::lex;
use bson::oid::ObjectId;
use bson::spec::BinarySubtype;
use bson::{Binary, Bson, DateTime, Regex, Timestamp, bson};

fn parse(src: &str) -> Result<Bson, ParseError> {
    let (toks, err) = lex(src);
    assert_eq!(err, None, "{src}");
    let mut c = Cursor::new(src, &toks);
    let v = value(&mut c)?;
    assert!(c.at_end(), "{src}: tokens left after the value");
    Ok(v)
}

fn ok(src: &str) -> Bson {
    parse(src).unwrap_or_else(|e| panic!("{src}: {}", e.message))
}

fn err(src: &str) -> ParseError {
    parse(src).err().unwrap_or_else(|| panic!("{src} parsed"))
}

#[test]
fn an_integer_in_32_bit_range_is_int32_and_anything_else_double() {
    for (src, want) in [
        ("1", Bson::Int32(1)),
        ("2.0", Bson::Int32(2)),
        ("1e3", Bson::Int32(1000)),
        ("-5", Bson::Int32(-5)),
        ("2147483647", Bson::Int32(i32::MAX)),
        ("-2147483648", Bson::Int32(i32::MIN)),
        ("2147483648", Bson::Double(2147483648.0)),
        ("-2147483649", Bson::Double(-2147483649.0)),
        ("3000000000", Bson::Double(3e9)),
        ("1.5", Bson::Double(1.5)),
        ("1e-3", Bson::Double(0.001)),
    ] {
        assert_eq!(ok(src), want, "{src}");
    }
}

#[test]
fn objects_and_arrays_keep_order_and_take_every_key_form() {
    for (src, want) in [
        (
            "{b: 1, 'a': 'x', \"$c\": [true, false, null,], $match: {}, 1: 2,}",
            bson!({ "b": 1, "a": "x", "$c": [true, false, null], "$match": {}, "1": 2 }),
        ),
        ("{a: 1, a: 2, b: 3}", bson!({ "a": 2, "b": 3 })),
        ("[]", bson!([])),
        ("[[1], {a: [2]}]", bson!([[1], { "a": [2] }])),
    ] {
        assert_eq!(ok(src), want, "{src}");
    }
}

#[test]
fn shell_helpers_build_their_bson_types() {
    let oid = ObjectId::parse_str("65a1b2c3d4e5f60718293a4b").unwrap();
    let at = Bson::DateTime(DateTime::from_millis(1_704_164_645_000));
    let uuid = bson::Uuid::parse_str("00112233-4455-6677-8899-aabbccddeeff").unwrap();
    let dec = Bson::Decimal128("1.10".parse().unwrap());
    let bin = |subtype: BinarySubtype, bytes: Vec<u8>| Bson::Binary(Binary { subtype, bytes });
    let re = |p: &str, o: &str| {
        Bson::RegularExpression(Regex {
            pattern: p.try_into().unwrap(),
            options: o.try_into().unwrap(),
        })
    };
    let ts = Bson::Timestamp(Timestamp {
        time: 1,
        increment: 2,
    });
    for (src, want) in [
        ("ObjectId('65a1b2c3d4e5f60718293a4b')", Bson::ObjectId(oid)),
        ("ISODate('2024-01-02T03:04:05Z')", at.clone()),
        ("ISODate(\"2024-01-02T03:04:05\")", at.clone()),
        ("ISODate('2024-01-02T05:04:05+02:00')", at.clone()),
        ("new Date('2024-01-02T03:04:05Z')", at.clone()),
        ("new Date(1704164645000)", at),
        (
            "ISODate('2024-01-02')",
            Bson::DateTime(DateTime::from_millis(1_704_153_600_000)),
        ),
        (
            "ISODate('2024-01-02T03:04:05.123Z')",
            Bson::DateTime(DateTime::from_millis(1_704_164_645_123)),
        ),
        ("NumberInt(7)", Bson::Int32(7)),
        ("NumberInt('7')", Bson::Int32(7)),
        ("Int32(7)", Bson::Int32(7)),
        (
            "NumberLong('9007199254740993')",
            Bson::Int64(9_007_199_254_740_993),
        ),
        ("NumberLong(5)", Bson::Int64(5)),
        ("Long('-3')", Bson::Int64(-3)),
        ("Double(1)", Bson::Double(1.0)),
        ("Double('2.5')", Bson::Double(2.5)),
        ("NumberDecimal('1.10')", dec.clone()),
        ("Decimal128(\"1.10\")", dec.clone()),
        ("NumberDecimal(1.10)", dec),
        (
            "UUID('00112233-4455-6677-8899-aabbccddeeff')",
            Bson::Binary(Binary::from_uuid(uuid)),
        ),
        (
            "BinData(0, 'AQID')",
            bin(BinarySubtype::Generic, vec![1, 2, 3]),
        ),
        ("BinData(128, '')", bin(BinarySubtype::from(128u8), vec![])),
        ("Timestamp(1, 2)", ts.clone()),
        ("Timestamp({t: 1, i: 2})", ts),
        ("MinKey()", Bson::MinKey),
        ("MaxKey()", Bson::MaxKey),
        ("/ab+c/mi", re("ab+c", "im")),
        (r"/x\/y/", re(r"x\/y", "")),
        ("RegExp('a.c', 'xi')", re("a.c", "ix")),
        ("RegExp('a.c')", re("a.c", "")),
    ] {
        assert_eq!(ok(src), want, "{src}");
    }
    assert!(matches!(ok("ObjectId()"), Bson::ObjectId(_)));
    assert!(matches!(ok("new Date()"), Bson::DateTime(_)));
    assert!(matches!(
        ok("UUID()"),
        Bson::Binary(Binary {
            subtype: BinarySubtype::Uuid,
            ..
        })
    ));
}

#[test]
fn javascript_beyond_literals_is_refused_with_one_message_at_its_token() {
    for (src, at) in [
        ("foo", 0),
        ("undefined", 0),
        ("{a: b}", 4),
        ("{a: 1 + 2}", 6),
        ("[1, -x]", 4),
        ("print('x')", 0),
        ("Date('2024-01-01')", 0),
        ("new Foo()", 4),
        ("function () {}", 0),
        ("{a}", 1),
        ("`t`", 0),
        ("(1)", 0),
        ("{a: x => x}", 4),
    ] {
        let e = err(src);
        assert_eq!((e.message.as_str(), e.at), (NOT_JS, at), "{src}");
    }
}

#[test]
fn a_bad_helper_argument_names_the_helper() {
    for src in [
        "ObjectId('zz')",
        "ObjectId(1)",
        "ISODate('yesterday')",
        "NumberInt(3000000000)",
        "NumberInt(1.5)",
        "NumberLong('x')",
        "NumberDecimal('abc')",
        "UUID('nope')",
        "BinData(256, 'AQ==')",
        "BinData(0, '!!')",
        "Timestamp(-1, 0)",
        "Timestamp({t: 1})",
        "MinKey(1)",
        "RegExp(1)",
        "Double('x')",
        "new Date(1.5)",
    ] {
        let e = err(src);
        let name = src.trim_start_matches("new ").split('(').next().unwrap();
        assert!(
            e.message.contains(name) && e.message != NOT_JS,
            "{src}: {}",
            e.message
        );
    }
    assert_eq!(
        err("ObjectId('zz')").at,
        0,
        "a helper error points at its name"
    );
    assert!(err("/a/g").message.contains("`g`"));
}

#[test]
fn structural_errors_say_what_was_expected() {
    for (src, want, at) in [
        ("{a: 1 b: 2}", "expected `,` or `}`", 6),
        ("[1 2]", "expected `,` or `]`", 3),
        ("{a 1}", "expected `:`", 3),
        ("{a: 1", "expected `,` or `}`", 5),
        ("{: 1}", "expected a key", 1),
        ("{a:", "expected a value", 3),
    ] {
        let e = err(src);
        assert_eq!((e.message.as_str(), e.at), (want, at), "{src}");
    }
}

#[test]
fn nesting_past_bsons_limit_and_a_nul_in_a_regex_are_refused() {
    let deep = |n: usize| format!("{}{}", "[".repeat(n), "]".repeat(n));
    assert_eq!(
        ok(&deep(100)),
        (0..99).fold(bson!([]), |inner, _| bson!([inner]))
    );
    let e = err(&deep(101));
    assert_eq!(
        (e.message.as_str(), e.at),
        ("nested deeper than 100 levels", 100)
    );
    assert_eq!(
        err(&"NumberInt(".repeat(200)).message,
        "nested deeper than 100 levels",
        "helper calls nest too"
    );
    assert_eq!(
        err("RegExp('a\\0b')").message,
        "regex pattern holds a NUL byte"
    );
}
