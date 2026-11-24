use ::redis::parse_redis_value;

use super::fields_json;

#[test]
fn stream_fields_are_a_json_object_and_bytes_are_base64() {
    let flat =
        parse_redis_value(b"*4\r\n$1\r\nf\r\n$1\r\nv\r\n$1\r\nb\r\n$2\r\n\xff\x00\r\n").unwrap();
    let ::redis::Value::Array(items) = flat else {
        panic!()
    };
    assert_eq!(fields_json(&items), r#"{"f":"v","b":{"$bytes":"/wA="}}"#);
}

#[test]
fn a_repeated_field_name_keeps_every_pair_in_order() {
    let flat = parse_redis_value(
        b"*6\r\n$1\r\nf\r\n$1\r\na\r\n$1\r\nf\r\n$1\r\nb\r\n$1\r\ng\r\n$1\r\nc\r\n",
    )
    .unwrap();
    let ::redis::Value::Array(items) = flat else {
        panic!()
    };
    assert_eq!(
        fields_json(&items),
        r#"{"$pairs":[["f","a"],["f","b"],["g","c"]]}"#
    );
}

#[test]
fn a_repeated_binary_field_name_is_tagged_inside_pairs() {
    let flat =
        parse_redis_value(b"*4\r\n$2\r\n\xff\x00\r\n$1\r\na\r\n$2\r\n\xff\x00\r\n$1\r\nb\r\n")
            .unwrap();
    let ::redis::Value::Array(items) = flat else {
        panic!()
    };
    assert_eq!(
        fields_json(&items),
        r#"{"$pairs":[[{"$bytes":"/wA="},"a"],[{"$bytes":"/wA="},"b"]]}"#
    );
}
