use bson::oid::ObjectId;
use bson::spec::BinarySubtype;
use bson::{Binary, Bson, DateTime, Decimal128, JavaScriptCodeWithScope, RawDocument, Regex};
use bson::{Timestamp, doc};

use super::*;

fn text(s: &str) -> CellText {
    CellText::Text(s.to_owned())
}

fn number(s: &str) -> CellText {
    CellText::Number(s.to_owned())
}

#[test]
fn scalars_are_bare_in_a_cell() {
    assert_eq!(cell(&Bson::String("it's".into())), text("it's"));
    assert_eq!(cell(&Bson::Null), CellText::Null);
    assert_eq!(cell(&Bson::Boolean(true)), text("true"));
    assert_eq!(cell(&Bson::Int32(-7)), number("-7"));
    assert_eq!(
        cell(&Bson::Int64(9007199254740993)),
        number("9007199254740993")
    );
    assert_eq!(cell(&Bson::Double(1.5)), number("1.5"));
    assert_eq!(cell(&Bson::Double(2.0)), number("2"));
    assert_eq!(cell(&Bson::Double(1e300)), number("1e+300"));
    assert_eq!(cell(&Bson::Double(-1.5e300)), number("-1.5e+300"));
    assert_eq!(cell(&Bson::Double(1e21)), number("1e+21"));
    assert_eq!(cell(&Bson::Double(1e-7)), number("1e-7"));
    assert_eq!(cell(&Bson::Double(f64::NAN)), number("NaN"));
    assert_eq!(cell(&Bson::Double(f64::NEG_INFINITY)), number("-Infinity"));
    let d: Decimal128 = "1.10".parse().unwrap();
    assert_eq!(cell(&Bson::Decimal128(d)), number("1.10"));
}

#[test]
fn every_other_type_is_mongosh_syntax() {
    let oid = ObjectId::parse_str("507f1f77bcf86cd799439011").unwrap();
    let bin = |subtype, bytes: &[u8]| {
        Bson::Binary(Binary {
            subtype,
            bytes: bytes.to_vec(),
        })
    };
    let date = |ms| Bson::DateTime(DateTime::from_millis(ms));
    let re = Regex {
        pattern: "a.c".try_into().unwrap(),
        options: "im".try_into().unwrap(),
    };
    let uuid = bson::Uuid::parse_str("01234567-89ab-cdef-0123-456789abcdef").unwrap();
    let cases: Vec<(Bson, &str)> = vec![
        (Bson::ObjectId(oid), "ObjectId('507f1f77bcf86cd799439011')"),
        (date(1704164645000), "ISODate('2024-01-02T03:04:05.000Z')"),
        (date(1704164645120), "ISODate('2024-01-02T03:04:05.120Z')"),
        (date(-1000), "ISODate('1969-12-31T23:59:59.000Z')"),
        (date(-1), "ISODate('1969-12-31T23:59:59.999Z')"),
        (date(253402300799999), "ISODate('9999-12-31T23:59:59.999Z')"),
        (date(253402300800000), "new Date(253402300800000)"),
        (date(-62167219200000), "ISODate('0000-01-01T00:00:00.000Z')"),
        (date(-62167219200001), "new Date(-62167219200001)"),
        (
            bin(BinarySubtype::Generic, &[1, 2, 3]),
            "Binary.createFromBase64('AQID', 0)",
        ),
        (
            bin(BinarySubtype::UserDefined(0x80), &[255]),
            "Binary.createFromBase64('/w==', 128)",
        ),
        (
            Bson::Binary(Binary::from_uuid(uuid)),
            "UUID('01234567-89ab-cdef-0123-456789abcdef')",
        ),
        (
            bin(BinarySubtype::Uuid, &[1]),
            "Binary.createFromBase64('AQ==', 4)",
        ),
        (
            bin(BinarySubtype::UuidOld, &[0; 16]),
            "Binary.createFromBase64('AAAAAAAAAAAAAAAAAAAAAA==', 3)",
        ),
        (
            Bson::Timestamp(Timestamp {
                time: 1,
                increment: 2,
            }),
            "Timestamp({ t: 1, i: 2 })",
        ),
        (Bson::RegularExpression(re), "/a.c/im"),
        (Bson::MinKey, "MinKey()"),
        (Bson::MaxKey, "MaxKey()"),
        (Bson::JavaScriptCode("x".into()), "Code('x')"),
        (
            Bson::JavaScriptCodeWithScope(JavaScriptCodeWithScope {
                code: "x".into(),
                scope: doc! { "x": 1 },
            }),
            "Code('x', { x: 1 })",
        ),
    ];
    for (value, want) in cases {
        assert_eq!(cell(&value), text(want));
        assert_eq!(shell(&value), want);
    }
}

#[test]
fn nested_values_are_compact_and_numbers_keep_their_type() {
    let d: Decimal128 = "1.10".parse().unwrap();
    let value = Bson::Document(doc! {
        "a": [1, { "b": 2 }],
        "n": Bson::Int64(5),
        "x": d,
        "dbl": 2.0,
        "s": "it's\n",
        "odd-key": null,
        "e": {},
        "l": [],
    });
    let want = "{ a: [ 1, { b: 2 } ], n: Long('5'), x: Decimal128('1.10'), dbl: 2, \
                s: 'it\\'s\\n', 'odd-key': null, e: {}, l: [] }";
    assert_eq!(shell(&value), want);
    assert_eq!(cell(&value), text(want));
}

#[test]
fn deprecated_types_are_canonical_extended_json_never_null() {
    assert_eq!(cell(&Bson::Undefined), text("{\"$undefined\":true}"));
    assert_eq!(cell(&Bson::Symbol("s".into())), text("{\"$symbol\":\"s\"}"));
    assert_eq!(
        shell(&Bson::Document(doc! { "u": Bson::Undefined })),
        "{ u: {\"$undefined\":true} }"
    );
}

#[test]
fn pretty_puts_each_field_on_its_own_line() {
    let d = doc! { "_id": 1, "o": { "a": [1, {}] }, "e": [] };
    assert_eq!(
        shell_pretty(&d),
        "{\n  _id: 1,\n  o: {\n    a: [\n      1,\n      {}\n    ]\n  },\n  e: []\n}"
    );
}

#[test]
fn extended_json_keeps_field_order() {
    let d = doc! { "z": 1, "a": 1.5, "m": { "y": 1, "b": 2 } };
    assert_eq!(
        extended_json(d.clone(), true),
        r#"{"z":{"$numberInt":"1"},"a":{"$numberDouble":"1.5"},"m":{"y":{"$numberInt":"1"},"b":{"$numberInt":"2"}}}"#
    );
    assert_eq!(
        extended_json(d, false),
        r#"{"z":1,"a":1.5,"m":{"y":1,"b":2}}"#
    );
}

#[test]
fn a_date_bson_cannot_spell_stays_exact_in_relaxed_json() {
    let d = doc! { "at": DateTime::from_millis(253402300800000), "in": [DateTime::from_millis(0)] };
    assert_eq!(
        extended_json(d, false),
        r#"{"at":{"$date":{"$numberLong":"253402300800000"}},"in":[{"$date":"1970-01-01T00:00:00Z"}]}"#
    );
}

#[test]
fn a_date_in_a_code_scope_stays_exact_in_relaxed_json() {
    let code = Bson::JavaScriptCodeWithScope(JavaScriptCodeWithScope {
        code: "x".into(),
        scope: doc! { "at": DateTime::from_millis(253402300800000) },
    });
    assert_eq!(
        extended_json(doc! { "c": code }, false),
        r#"{"c":{"$code":"x","$scope":{"at":{"$date":{"$numberLong":"253402300800000"}}}}}"#
    );
}

#[test]
fn invalid_utf8_is_replaced_not_refused() {
    // {a: "\xff"}: length 14, type 2, key "a", string length 2, the byte, NUL, end.
    let bytes = [14u8, 0, 0, 0, 2, b'a', 0, 2, 0, 0, 0, 0xff, 0, 0];
    let raw = RawDocument::from_bytes(&bytes).unwrap();
    assert_eq!(decode(raw).unwrap().get_str("a").unwrap(), "\u{fffd}");
}

#[test]
fn type_names_are_the_type_aliases() {
    let cases = [
        (Bson::Int32(1), "int"),
        (Bson::Int64(1), "long"),
        (Bson::Double(1.0), "double"),
        (Bson::String(String::new()), "string"),
        (Bson::Document(doc! {}), "object"),
        (Bson::Array(vec![]), "array"),
        (Bson::Boolean(true), "bool"),
        (Bson::Null, "null"),
        (Bson::Undefined, "undefined"),
        (Bson::MinKey, "minKey"),
    ];
    for (value, want) in cases {
        assert_eq!(type_name(&value), want);
    }
}

// One path of `levels` containers, the outermost a document; inside it every level is an
// array when `array`, else a document.
fn nested(levels: usize, array: bool) -> Vec<u8> {
    let (kind, key) = if array { (4u8, b'0') } else { (3u8, b'a') };
    let mut bytes = vec![5, 0, 0, 0, 0];
    for _ in 1..levels {
        let len = i32::try_from(4 + 3 + bytes.len() + 1).unwrap();
        let mut outer = len.to_le_bytes().to_vec();
        outer.extend([kind, key, 0]);
        outer.extend(&bytes);
        outer.push(0);
        bytes = outer;
    }
    bytes
}

fn refused(bytes: &[u8]) -> bool {
    let raw = RawDocument::from_bytes(bytes).unwrap();
    match decode(raw) {
        Ok(_) => false,
        Err(reason) => {
            assert!(reason.contains("nested deeper than 200"), "{reason}");
            true
        }
    }
}

#[test]
fn a_document_nested_a_thousand_deep_is_refused_not_overflowed() {
    assert!(refused(&nested(1000, false)));
}

#[test]
fn arrays_nested_a_thousand_deep_are_refused_not_overflowed() {
    assert!(refused(&nested(1000, true)));
}

#[test]
fn two_hundred_levels_decode_and_render_and_one_more_is_refused() {
    for array in [false, true] {
        let bytes = nested(200, array);
        let document = decode(RawDocument::from_bytes(&bytes).unwrap()).unwrap();
        assert!(!shell_pretty(&document).is_empty());
        assert!(!extended_json(document, false).is_empty());
        assert!(refused(&nested(201, array)));
    }
}

#[test]
fn a_scope_nested_a_thousand_deep_is_refused_not_overflowed() {
    // {c: Code('x', <scope>)}: total length, code length 2, "x", NUL, then the scope document.
    let scope = nested(1000, false);
    let mut value = i32::try_from(4 + 4 + 2 + scope.len())
        .unwrap()
        .to_le_bytes()
        .to_vec();
    value.extend([2, 0, 0, 0, b'x', 0]);
    value.extend(&scope);
    let mut bytes = i32::try_from(4 + 3 + value.len() + 1)
        .unwrap()
        .to_le_bytes()
        .to_vec();
    bytes.extend([0x0f, b'c', 0]);
    bytes.extend(&value);
    bytes.push(0);
    assert!(refused(&bytes));
}
