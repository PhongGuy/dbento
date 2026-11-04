use std::borrow::Cow;

use super::{Cell, cell, surql_text};
use crate::surreal::decode::{Wire, decode};
use crate::value::CellRef;

fn wire(hex_text: &str) -> Wire {
    decode(&hex::decode(hex_text).unwrap()).unwrap()
}

fn text(hex_text: &str) -> String {
    surql_text(&wire(hex_text))
}

fn shown(hex_text: &str) -> String {
    let v = wire(hex_text);
    match cell(&v) {
        Cell::Text(t) => t.into_owned(),
        other => panic!("{hex_text}: {other:?}"),
    }
}

fn number(hex_text: &str) -> String {
    let v = wire(hex_text);
    match cell(&v) {
        Cell::Number(t) => t.into_owned(),
        other => panic!("{hex_text}: {other:?}"),
    }
}

#[test]
fn none_null_and_absent_are_null_cells() {
    for hex_text in ["c6f6", "f6", "f7"] {
        assert_eq!(cell(&wire(hex_text)), Cell::Null, "{hex_text}");
    }
}

#[test]
fn numbers_show_their_digits_and_set_the_number_bit() {
    for (hex_text, want) in [
        ("00", "0"),
        ("1b7fffffffffffffff", "9223372036854775807"),
        ("3b7fffffffffffffff", "-9223372036854775808"),
        ("f93c00", "1.0"),
        ("f93e00", "1.5"),
        ("fb3fb999999999999a", "0.1"),
        ("fa5a000000", "9007199254740992.0"),
        ("f98000", "-0.0"),
        ("ca63312e35", "1.5"),
    ] {
        assert_eq!(number(hex_text), want, "{hex_text}");
    }
    assert!(cell(&wire("00")).is_number());
    assert!(!cell(&wire("f5")).is_number());
    assert!(!cell(&wire("c6f6")).is_number());
}

#[test]
fn floats_from_1e16_up_take_the_exponent_form() {
    assert_eq!(number("fb4341c37937e08000"), "1e16");
    assert_eq!(number("fb4415af1d78b58c40"), "1e20");
    assert_eq!(shown("81fb4341c37937e08000"), "[1e16]");
}

#[test]
fn nan_and_the_infinities_are_text_not_numbers() {
    assert_eq!(shown("f97e00"), "NaN");
    assert_eq!(shown("f97c00"), "Infinity");
    assert_eq!(shown("f9fc00"), "-Infinity");
}

#[test]
fn bools_strings_and_bytes_are_as_they_came() {
    assert_eq!(shown("f5"), "true");
    assert_eq!(shown("f4"), "false");
    let s = wire("63610062");
    assert_eq!(cell(&s), Cell::Text(Cow::Borrowed("a\0b")));
    let b = wire("426162");
    assert_eq!(cell(&b), Cell::Bytes(b"ab"));
    assert_eq!(cell(&b).as_ref(), CellRef::Bytes(b"ab"));
    assert_eq!(cell(&s).as_ref(), CellRef::Text("a\0b"));
    assert_eq!(Cell::Null.as_ref(), CellRef::Null);
}

#[test]
fn datetimes_are_rfc_3339_with_full_nanos_or_their_raw_parts() {
    for (hex_text, want) in [
        ("cc820000", "1970-01-01T00:00:00.000000000Z"),
        ("cc82201a1dcd6500", "1969-12-31T23:59:59.500000000Z"),
        ("cc823b0000000e7791f6ff00", "0001-01-01T00:00:00.000000000Z"),
        ("cc823b000007948cf211ff00", "-8334601228800s 0ns"),
        (
            "cc821b000007779a0a6b7f1a3b9ac9ff",
            "8210266876799s 999999999ns",
        ),
    ] {
        assert_eq!(shown(hex_text), want, "{hex_text}");
    }
}

#[test]
fn a_datetime_past_the_formatter_keeps_its_parts_at_every_depth() {
    for (hex_text, want) in [
        ("cc821b7fffffffffffffff00", "9223372036854775807s 0ns"),
        ("cc823b7fffffffffffffff00", "-9223372036854775808s 0ns"),
    ] {
        assert_eq!(shown(hex_text), want, "{hex_text}");
    }
    assert_eq!(
        shown("82cc823b000007948cf211ff00cc821b000007779a0a6b7f1a3b9ac9ff"),
        "[-8334601228800s 0ns, 8210266876799s 999999999ns]"
    );
}

#[test]
fn the_edges_of_the_four_digit_years_are_still_rfc_3339() {
    for (hex_text, want) in [
        ("cc823b0000000e79747bff00", "0000-01-01T00:00:00.000000000Z"),
        (
            "cc821b0000003afff4417f1a3b9ac9ff",
            "9999-12-31T23:59:59.999999999Z",
        ),
        ("cc823b0000000e79747c0000", "-62167219201s 0ns"),
        ("cc821b0000003afff4418000", "253402300800s 0ns"),
    ] {
        assert_eq!(shown(hex_text), want, "{hex_text}");
    }
}

#[test]
fn durations_are_surrealql_text() {
    for (hex_text, want) in [
        ("ce80", "0ns"),
        ("ce81190e10", "1h"),
        ("ce82001a11e1a300", "300ms"),
        ("ce82011a1dcd6500", "1s500ms"),
        ("ce81184d", "1m17s"),
        ("ce82001905dc", "1µs500ns"),
        (
            "ce821bffffffffffffffff1a3b9ac9ff",
            "584942417355y3w5d7h15s999ms999µs999ns",
        ),
    ] {
        assert_eq!(shown(hex_text), want, "{hex_text}");
    }
}

#[test]
fn uuids_tables_records_and_ranges_are_surrealql_text() {
    let id = "018f0000-0000-7000-8000-000000000001";
    assert_eq!(shown("d82550018f0000000070008000000000000001"), id);
    assert_eq!(shown("c76174"), "t");
    for (hex_text, want) in [
        ("c882616101", "a:1".to_owned()),
        ("c882616163616263", "a:abc".to_owned()),
        ("c88261616131", "a:⟨1⟩".to_owned()),
        ("c8826361206263633b64", "⟨a b⟩:⟨c;d⟩".to_owned()),
        ("c88261616578e29fa979", "a:`x⟩y`".to_owned()),
        (
            "c8826161d82550018f0000000070008000000000000001",
            format!("a:u'{id}'"),
        ),
        ("c8826161a2616202617801", "a:{b: 2, x: 1}".to_owned()),
        ("c882617782614c02", "w:['L', 2]".to_owned()),
        ("c8826161d83182d83301d83205", "a:1>..=5".to_owned()),
        ("d83182d83201d83305", "1..5".to_owned()),
        ("d83182f6f6", "..".to_owned()),
        ("d83182d83201f6", "1..".to_owned()),
        ("d83182f6d83205", "..=5".to_owned()),
    ] {
        assert_eq!(shown(hex_text), want, "{hex_text}");
    }
}

#[test]
fn geometries_sets_and_files_are_surrealql_text() {
    for (hex_text, want) in [
        ("d85882f93c00f94000", "(1.0, 2.0)"),
        (
            "d85982d85882f93c00f94000d85882f94200f94400",
            "{type: 'LineString', coordinates: [[1.0, 2.0], [3.0, 4.0]]}",
        ),
        (
            "d85a81d85983d85882f90000f90000d85882f93c00f90000d85882f90000f90000",
            "{type: 'Polygon', coordinates: [[[0.0, 0.0], [1.0, 0.0], [0.0, 0.0]]]}",
        ),
        (
            "d85b81d85882f93c00f94000",
            "{type: 'MultiPoint', coordinates: [[1.0, 2.0]]}",
        ),
        (
            "d85c81d85982d85882f93c00f94000d85882f94200f94400",
            "{type: 'MultiLineString', coordinates: [[[1.0, 2.0], [3.0, 4.0]]]}",
        ),
        (
            "d85d81d85a81d85982d85882f90000f90000d85882f93c00f90000",
            "{type: 'MultiPolygon', coordinates: [[[[0.0, 0.0], [1.0, 0.0]]]]}",
        ),
        (
            "d85e81d85882f93c00f94000",
            "{type: 'GeometryCollection', geometries: [(1.0, 2.0)]}",
        ),
        ("d85e80", "{type: 'GeometryCollection', geometries: []}"),
        ("d838820102", "{1, 2}"),
        ("d8388109", "{9,}"),
        ("d83880", "{,}"),
        ("d8378262626b662f612e747874", "f'bk:/a.txt'"),
    ] {
        assert_eq!(shown(hex_text), want, "{hex_text}");
    }
}

#[test]
fn objects_and_arrays_are_compact_with_none_and_null_distinct() {
    assert_eq!(
        shown("a16161a16162a161638201a16164c6f6"),
        "{a: {b: {c: [1, {d: NONE}]}}}"
    );
    assert_eq!(
        shown(
            "89c6f6f66469742773cc820000d82550018f0000000070008000000000000001426162ca63312e35f93c00f5"
        ),
        "[NONE, NULL, 'it\\'s', d'1970-01-01T00:00:00.000000000Z', \
         u'018f0000-0000-7000-8000-000000000001', b'6162', 1.5dec, 1.0, true]"
    );
    assert_eq!(shown("a26361206201616302"), "{'a b': 1, c: 2}");
    assert_eq!(text("c6f6"), "NONE");
    assert_eq!(text("f6"), "NULL");
}

#[test]
fn an_unknown_tag_shows_cbor_diagnostic_text_and_never_null() {
    for (hex_text, want) in [
        ("d903e701", "999(1)"),
        ("d903e76178", "999(\"x\")"),
        ("d903e7426162", "999(h'6162')"),
        ("d903e78201f6", "999([1, null])"),
        ("d903e7a16161f5", "999({\"a\": true})"),
        ("d903e8d903e701", "1000(999(1))"),
        ("c96178", "9(\"x\")"),
        ("c24101", "2(h'01')"),
        ("d839420101", "57(h'0101')"),
        ("81d903e701", "[999(1)]"),
    ] {
        assert_eq!(shown(hex_text), want, "{hex_text}");
    }
}

#[test]
fn a_known_value_inside_an_unknown_tag_keeps_its_surrealql_text() {
    assert_eq!(
        shown("d903e7cc820000"),
        "999(d'1970-01-01T00:00:00.000000000Z')"
    );
    assert_eq!(shown("d903e781c6f6"), "999([NONE])");
}

#[test]
fn a_set_opening_with_a_record_is_a_cast_because_braces_would_read_as_an_object() {
    assert_eq!(shown("d83882c882616101c882616202"), "<set>[a:1, b:2]");
    assert_eq!(shown("d83881c882616101"), "<set>[a:1]");
    assert_eq!(shown("d8388201c882616101"), "{1, a:1}");
}

#[test]
fn a_nested_table_is_a_cast_because_a_bare_name_reads_as_a_field() {
    assert_eq!(shown("81c76174"), "[<table>'t']");
    assert_eq!(shown("81c763612762"), "[<table>'a\\'b']");
}

#[test]
fn text_bounds_are_names_in_a_record_key_and_strings_elsewhere() {
    assert_eq!(shown("c8826161d83182d8326178d8336179"), "a:x..y");
    assert_eq!(shown("c8826161d83182d83263612062d8336179"), "a:⟨a b⟩..y");
    assert_eq!(shown("d83182d8326178d8336179"), "'x'..'y'");
}
