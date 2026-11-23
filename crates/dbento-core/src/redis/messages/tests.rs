use ::redis::{Value, parse_redis_value};

use super::{from_failure, xread_entries};
use crate::message::MessageError;
use crate::redis::driver::Failure;

#[test]
fn xread_answers_a_map_under_resp3_and_an_array_under_resp2() {
    let resp3 = b"%1\r\n$1\r\ng\r\n*1\r\n*2\r\n$3\r\n1-4\r\n*2\r\n$1\r\nn\r\n$1\r\n4\r\n";
    let resp2 = b"*1\r\n*2\r\n$1\r\ng\r\n*1\r\n*2\r\n$3\r\n1-4\r\n*2\r\n$1\r\nn\r\n$1\r\n4\r\n";
    for bytes in [&resp3[..], &resp2[..]] {
        let entries = xread_entries(parse_redis_value(bytes).unwrap()).unwrap();
        assert_eq!(entries.len(), 1);
        let Value::Array(parts) = &entries[0] else {
            panic!("{:?}", entries[0])
        };
        assert_eq!(parts[0], Value::BulkString(b"1-4".to_vec()));
    }
}

#[test]
fn a_timed_out_xread_is_no_entries() {
    for bytes in [&b"_\r\n"[..], &b"*-1\r\n"[..]] {
        assert!(
            xread_entries(parse_redis_value(bytes).unwrap())
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
fn a_failed_round_trip_maps_to_its_own_error() {
    assert!(matches!(
        from_failure(Failure::Timeout),
        MessageError::Timeout
    ));
    assert!(matches!(
        from_failure(Failure::Cancelled),
        MessageError::Cancelled
    ));
    assert!(matches!(
        from_failure(Failure::Ended("x".into())),
        MessageError::SessionClosed
    ));
}
