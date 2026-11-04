use std::ops::Bound;

use super::{DecodeFailure, Wire, decode, decode_bounded};

fn wire(hex_text: &str) -> Wire {
    decode(&hex::decode(hex_text).unwrap()).unwrap_or_else(|e| panic!("{hex_text}: {e}"))
}

fn failure(hex_text: &str) -> DecodeFailure {
    match decode(&hex::decode(hex_text).unwrap()) {
        Err(e) => e,
        Ok(v) => panic!("{hex_text} decoded to {v:?}"),
    }
}

fn point(a: f64, b: f64) -> Wire {
    Wire::Geometry {
        tag: 88,
        items: vec![Wire::Float(a), Wire::Float(b)],
    }
}

// Protocol probe B: null, NONE and undefined stay three values.
#[test]
fn none_null_and_undefined_stay_apart() {
    assert_eq!(wire("c6f6"), Wire::None, "6(null)");
    assert_eq!(wire("f6"), Wire::Null, "null");
    assert_eq!(wire("f7"), Wire::Undefined, "undefined");
    assert_eq!(wire("f5"), Wire::Bool(true));
    assert_eq!(wire("f4"), Wire::Bool(false));
    // A tag 6 payload is ignored: the probe's parameter table takes any.
    assert_eq!(wire("c601"), Wire::None, "6(1)");
}

#[test]
fn integers_keep_every_digit_at_the_extremes() {
    assert_eq!(wire("00"), Wire::Int(0));
    assert_eq!(
        wire("1b7fffffffffffffff"),
        Wire::Int(9_223_372_036_854_775_807)
    );
    assert_eq!(
        wire("3b7fffffffffffffff"),
        Wire::Int(-9_223_372_036_854_775_808)
    );
    assert_eq!(
        wire("1bffffffffffffffff"),
        Wire::Int(18_446_744_073_709_551_615)
    );
    assert_eq!(
        wire("3bffffffffffffffff"),
        Wire::Int(-18_446_744_073_709_551_616)
    );
}

#[test]
fn all_three_float_widths_decode() {
    assert_eq!(wire("f93c00"), Wire::Float(1.0), "1.0_f16");
    assert_eq!(wire("fb3fb999999999999a"), Wire::Float(0.1), "0.1_f64");
    assert_eq!(
        wire("fa5a000000"),
        Wire::Float(9_007_199_254_740_992.0),
        "_f32"
    );
    assert_eq!(wire("f97c00"), Wire::Float(f64::INFINITY), "inf_f16");
    assert_eq!(wire("f9fc00"), Wire::Float(f64::NEG_INFINITY), "-inf_f16");
    let Wire::Float(nan) = wire("f97e00") else {
        panic!("nan_f16")
    };
    assert!(nan.is_nan());
    let Wire::Float(zero) = wire("f98000") else {
        panic!("-0.0_f16")
    };
    assert!(zero == 0.0 && zero.is_sign_negative());
}

#[test]
fn text_bytes_and_decimals_decode() {
    assert_eq!(
        wire("ca63312e35"),
        Wire::Decimal("1.5".into()),
        "10(\"1.5\")"
    );
    assert_eq!(wire("63610062"), Wire::Text("a\0b".into()), "NUL is kept");
    assert_eq!(wire("426162"), Wire::Bytes(b"ab".to_vec()));
    assert_eq!(wire("c76174"), Wire::Table("t".into()), "7(\"t\")");
    assert_eq!(
        wire("d8378262626b662f612e747874"),
        Wire::File {
            bucket: "bk".into(),
            path: "/a.txt".into()
        },
        "55([\"bk\", \"/a.txt\"])"
    );
}

#[test]
fn datetimes_decode_to_seconds_and_nanos() {
    for (hex_text, secs, nanos) in [
        ("cc820000", 0, 0),
        ("cc82201a1dcd6500", -1, 500_000_000),
        ("cc823b0000000e7791f6ff00", -62_135_596_800, 0),
        ("cc823b000007948cf211ff00", -8_334_601_228_800, 0),
        (
            "cc821b000007779a0a6b7f1a3b9ac9ff",
            8_210_266_876_799,
            999_999_999,
        ),
    ] {
        assert_eq!(wire(hex_text), Wire::Datetime { secs, nanos }, "{hex_text}");
    }
}

#[test]
fn every_duration_form_decodes() {
    for (hex_text, secs, nanos) in [
        ("ce80", 0, 0),
        ("ce81190e10", 3600, 0),
        ("ce82001a11e1a300", 0, 300_000_000),
        ("ce82011a1dcd6500", 1, 500_000_000),
        ("ce821bffffffffffffffff1a3b9ac9ff", u64::MAX, 999_999_999),
    ] {
        assert_eq!(wire(hex_text), Wire::Duration { secs, nanos }, "{hex_text}");
    }
}

#[test]
fn uuids_and_record_ids_decode() {
    let id: [u8; 16] = hex::decode("018f0000000070008000000000000001")
        .unwrap()
        .try_into()
        .unwrap();
    assert_eq!(
        wire("d82550018f0000000070008000000000000001"),
        Wire::Uuid(id)
    );
    let record = |key| Wire::Record {
        table: "a".into(),
        key: Box::new(key),
    };
    assert_eq!(wire("c882616101"), record(Wire::Int(1)), "a:1");
    assert_eq!(
        wire("c88261616131"),
        record(Wire::Text("1".into())),
        "a:⟨1⟩"
    );
    assert_eq!(
        wire("c8826161d82550018f0000000070008000000000000001"),
        record(Wire::Uuid(id)),
        "a:u'...'"
    );
    assert_eq!(
        wire("c8826161a2616202617801"),
        record(Wire::Object(vec![
            ("b".into(), Wire::Int(2)),
            ("x".into(), Wire::Int(1))
        ])),
        "a:{{x:1,b:2}}"
    );
    assert_eq!(
        wire("c8826161d83182d83301d83205"),
        record(Wire::Range(Box::new((
            Bound::Excluded(Wire::Int(1)),
            Bound::Included(Wire::Int(5))
        )))),
        "a:1>..=5"
    );
}

#[test]
fn ranges_decode_with_every_kind_of_end() {
    let range = |a, b| Wire::Range(Box::new((a, b)));
    assert_eq!(
        wire("d83182d83201d83305"),
        range(Bound::Included(Wire::Int(1)), Bound::Excluded(Wire::Int(5))),
        "1..5"
    );
    assert_eq!(
        wire("d83182f6f6"),
        range(Bound::Unbounded, Bound::Unbounded)
    );
    assert_eq!(
        wire("d83182d83201f6"),
        range(Bound::Included(Wire::Int(1)), Bound::Unbounded)
    );
    assert_eq!(
        wire("d83182f6d83205"),
        range(Bound::Unbounded, Bound::Included(Wire::Int(5)))
    );
}

// Protocol probe B: only a bare null is unbounded, so a tagged end keeps its own value.
#[test]
fn a_range_end_tells_unbounded_from_a_null_or_none_value() {
    let range = |a, b| Wire::Range(Box::new((a, b)));
    assert_eq!(
        wire("d83182d832f6f6"),
        range(Bound::Included(Wire::Null), Bound::Unbounded),
        "49([50(null), null])"
    );
    assert_eq!(
        wire("d83182d832c6f6d83305"),
        range(Bound::Included(Wire::None), Bound::Excluded(Wire::Int(5))),
        "NONE..5"
    );
}

#[test]
fn geometries_and_sets_decode() {
    assert_eq!(
        wire("d85882f93c00f94000"),
        point(1.0, 2.0),
        "88([1.0_f16, 2.0_f16])"
    );
    assert_eq!(
        wire("d85982d85882f93c00f94000d85882f94200f94400"),
        Wire::Geometry {
            tag: 89,
            items: vec![point(1.0, 2.0), point(3.0, 4.0)]
        }
    );
    assert_eq!(
        wire("d85e80"),
        Wire::Geometry {
            tag: 94,
            items: vec![]
        },
        "94([]) is allowed"
    );
    assert_eq!(
        wire("d838820102"),
        Wire::Set(vec![Wire::Int(1), Wire::Int(2)]),
        "56([1, 2])"
    );
}

#[test]
fn containers_keep_nested_tags_and_the_wire_order() {
    // The probe's `{"a": {"b": {"c": [1, {"d": 6(null)}]}}}`.
    let d = Wire::Object(vec![("d".into(), Wire::None)]);
    let c = Wire::Object(vec![("c".into(), Wire::Array(vec![Wire::Int(1), d]))]);
    let b = Wire::Object(vec![("b".into(), c)]);
    assert_eq!(
        wire("a16161a16162a161638201a16164c6f6"),
        Wire::Object(vec![("a".into(), b)])
    );
    // The envelope from protocol probe A, byte for byte.
    assert_eq!(
        wire("a26269640166726573756c74c6f6"),
        Wire::Object(vec![
            ("id".into(), Wire::Int(1)),
            ("result".into(), Wire::None)
        ])
    );
}

#[test]
fn an_unknown_tag_keeps_its_number_and_payload() {
    assert_eq!(
        wire("d903e701"),
        Wire::Unknown {
            tag: 999,
            payload: Box::new(Wire::Int(1))
        }
    );
    // The wire forms that are parameters only, and v4's tags, are unknown here.
    for (hex_text, tag) in [("c96178", 9), ("c24101", 2), ("d839420101", 57)] {
        assert!(
            matches!(wire(hex_text), Wire::Unknown { tag: t, .. } if t == tag),
            "{hex_text}"
        );
    }
}

#[test]
fn a_known_tag_with_the_wrong_payload_fails() {
    for (hex_text, tag) in [
        ("cc8101", 12),
        ("cc82001a3b9aca00", 12),
        ("ce83010203", 14),
        ("ce8120", 14),
        ("ce82011a3b9aca00", 14),
        ("d8254100", 37),
        ("c863613a31", 8),
        ("d8378162626b", 55),
        ("ca01", 10),
        ("c701", 7),
        ("d831820102", 49),
        ("d85801", 88),
        ("d858820102", 88),
        ("d85883f93c00f94000f94200", 88),
        ("d83801", 56),
    ] {
        let m = failure(hex_text).message;
        assert!(m.contains(&format!("tag {tag}")), "{hex_text}: {m}");
    }
}

#[test]
fn truncated_or_malformed_input_fails_cleanly() {
    for hex_text in [
        "", "a16161", "636162", "cc", "f900", "fb00", "a1", "81", "19",
        // Invalid UTF-8, a map key that is not text, a simple value other than the four used.
        "61ff", "a10102", "f0", "f820", "ff",
    ] {
        failure(hex_text);
    }
    assert!(failure("0000").message.contains("follow"), "trailing bytes");
}

#[test]
fn indefinite_lengths_are_refused() {
    for hex_text in ["9f01ff", "bf616101ff", "7f6161ff", "5f4161ff"] {
        failure(hex_text);
    }
}

// Spec Verification: a hostile reply fails rather than exhausting memory.
#[test]
fn a_declared_length_past_the_reply_fails_before_allocating() {
    for hex_text in [
        "9b0000010000000000",
        "9bffffffffffffffff00",
        "bb0000010000000000",
        "bbffffffffffffffff00",
        "7b0000010000000000",
        "7bffffffffffffffff61",
        "5b0000010000000000",
        "5bffffffffffffffff61",
        "9a0000000200",
    ] {
        failure(hex_text);
    }
    let m = failure("9b0000010000000000").message;
    assert!(m.contains("declared length"), "{m}");
    // One map entry needs at least two bytes, so a one-byte tail cannot hold it.
    assert!(failure("a200").message.contains("declared length"));
}

#[test]
fn nesting_is_bounded_for_arrays_maps_and_tags() {
    let nested = |unit: &[u8], depth: usize| {
        let mut frame = unit.repeat(depth);
        frame.push(0);
        frame
    };
    decode(&nested(&[0x81], 200)).unwrap();
    decode(&nested(&[0xd8, 0x64], 200)).unwrap();
    for unit in [&[0x81u8][..], &[0xd8, 0x64], &[0xa1, 0x61, 0x61]] {
        let m = decode(&nested(unit, 300)).unwrap_err().message;
        assert!(m.contains("nesting"), "{unit:?}: {m}");
    }
    // Deep input of a hostile size stops at the bound, not at the stack.
    let m = decode(&nested(&[0x81], 5_000_000)).unwrap_err().message;
    assert!(m.contains("nesting"), "{m}");
}

#[test]
fn the_value_budget_stops_a_frame_of_tiny_items() {
    // `[0, 0, 0]` holds four values with its array.
    let frame = [0x83, 0, 0, 0];
    decode_bounded(&frame, 4).unwrap();
    let m = decode_bounded(&frame, 3).unwrap_err().message;
    assert!(m.contains("too many values"), "{m}");
}
