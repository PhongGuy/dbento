//! The cbor4ii 1.2.4 behaviours the SurrealDB decoder relies on. A failure after a bump means a
//! premise moved: re-plan, do not patch.
#![cfg(feature = "surrealdb")]

use cbor4ii::core::Value;
use cbor4ii::core::dec::{Decode, Read};
use cbor4ii::core::types;
use cbor4ii::core::utils::SliceReader;

fn reader(bytes: &[u8]) -> SliceReader<'_> {
    SliceReader::new(bytes)
}

// Why the decoder dispatches on a peeked byte: these would lose NONE from NULL.
#[test]
fn value_and_option_merge_null_with_undefined() {
    for byte in [0xf6, 0xf7] {
        assert_eq!(Value::decode(&mut reader(&[byte])).unwrap(), Value::Null);
        assert_eq!(Option::<u8>::decode(&mut reader(&[byte])).unwrap(), None);
    }
}

#[test]
fn each_float_decoder_takes_only_its_own_width() {
    let half_one = [0xf9, 0x3c, 0x00];
    let single_one = [0xfa, 0x3f, 0x80, 0x00, 0x00];
    let double_one = [0xfb, 0x3f, 0xf0, 0, 0, 0, 0, 0, 0];
    assert!(f64::decode(&mut reader(&half_one)).is_err());
    assert!(f64::decode(&mut reader(&single_one)).is_err());
    assert_eq!(f64::decode(&mut reader(&double_one)).unwrap(), 1.0);
    assert_eq!(f32::decode(&mut reader(&single_one)).unwrap(), 1.0);
    assert_eq!(
        f64::from(half::f16::decode(&mut reader(&half_one)).unwrap()),
        1.0
    );
}

#[test]
fn the_pull_api_peeks_counts_and_refuses_without_allocating() {
    let mut r = reader(&[0x82, 0x01]);
    assert_eq!(r.fill(1).unwrap().as_ref(), [0x82]);
    assert_eq!(
        r.fill(usize::MAX).unwrap().as_ref().len(),
        2,
        "peeking moved nothing"
    );
    assert_eq!(types::Array::<()>::len(&mut r).unwrap(), Some(2));
    // A declared length is only read: the caller bounds it.
    let huge = [0x9b, 0, 0, 1, 0, 0, 0, 0, 0];
    assert_eq!(
        types::Array::<()>::len(&mut reader(&huge)).unwrap(),
        Some(1 << 40)
    );
    assert_eq!(types::Array::<()>::len(&mut reader(&[0x9f])).unwrap(), None);
    assert_eq!(types::Map::<()>::len(&mut reader(&[0xbf])).unwrap(), None);
    assert!(<&str>::decode(&mut reader(&[0x7b, 0, 0, 1, 0, 0, 0, 0, 0])).is_err());
    assert_eq!(
        types::Tag::<()>::tag(&mut reader(&[0xd8, 0x25])).unwrap(),
        37
    );
    let types::Negative(n) = types::Negative::<u64>::decode(&mut reader(&[0x20])).unwrap();
    assert_eq!(n, 0);
}

#[test]
fn the_slice_reader_stops_stepping_in_at_256() {
    let mut r = reader(&[]);
    assert!((0..256).all(|_| r.step_in()));
    assert!(!r.step_in());
    r.step_out();
    assert!(r.step_in());
}
