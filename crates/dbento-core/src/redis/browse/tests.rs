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
