use ::redis::{Value, parse_redis_value};

use super::{from_failure, message_rows, xread_entries};
use crate::message::MessageError;
use crate::redis::browse::VALUE_CAP;
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

#[test]
fn a_fields_cell_cut_mid_char_keeps_only_whole_chars() {
    // `{"f":"` is six bytes, so the two-byte `é` starts on the last byte under the cap.
    let value = format!("{}é tail", "a".repeat(VALUE_CAP - 7));
    let entry = Value::Array(vec![
        Value::BulkString(b"1-1".to_vec()),
        Value::Array(vec![
            Value::BulkString(b"f".to_vec()),
            Value::BulkString(value.into_bytes()),
        ]),
    ]);
    let (rows, cut) = message_rows(Value::Array(vec![entry])).unwrap();
    assert!(cut, "a cell over the cap is reported as cut");
    let fields = &rows[0][2];
    assert_eq!(
        fields.len(),
        VALUE_CAP - 1,
        "the split char is dropped whole"
    );
    let text = std::str::from_utf8(fields).unwrap();
    assert!(
        text.ends_with('a'),
        "the cut ends on the last whole char: {:?}",
        &text[text.len() - 4..]
    );
}

#[test]
fn xinfo_groups_reads_each_map_and_a_nil_lag() {
    let bytes = b"*2\r\n%6\r\n$4\r\nname\r\n$2\r\ng1\r\n$9\r\nconsumers\r\n:1\r\n$7\r\npending\r\n:3\r\n\
                  $17\r\nlast-delivered-id\r\n$3\r\n1-3\r\n$12\r\nentries-read\r\n:3\r\n$3\r\nlag\r\n:2\r\n\
                  %6\r\n$4\r\nname\r\n$2\r\ng2\r\n$9\r\nconsumers\r\n:0\r\n$7\r\npending\r\n:0\r\n\
                  $17\r\nlast-delivered-id\r\n$3\r\n1-5\r\n$12\r\nentries-read\r\n_\r\n$3\r\nlag\r\n_\r\n";
    let groups = super::group_infos(parse_redis_value(bytes).unwrap()).unwrap();
    let got: Vec<_> = groups
        .iter()
        .map(|g| {
            (
                g.name.as_str(),
                g.consumers,
                g.pending,
                g.last_delivered.as_redis_id(),
                g.lag,
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            ("g1", 1, 3, Some("1-3"), Some(2)),
            ("g2", 0, 0, Some("1-5"), None)
        ]
    );
}

#[test]
fn xpending_rows_are_id_consumer_idle_and_deliveries() {
    let bytes = b"*1\r\n*4\r\n$3\r\n1-1\r\n$5\r\nalice\r\n:8710\r\n:1\r\n";
    let rows = super::pending_rows(parse_redis_value(bytes).unwrap()).unwrap();
    assert_eq!(
        rows,
        [[
            b"1-1".to_vec(),
            b"alice".to_vec(),
            b"8710".to_vec(),
            b"1".to_vec()
        ]]
    );
}
