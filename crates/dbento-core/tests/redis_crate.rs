//! What Dbento relies on in redis 1.8.1's RESP3 parser, checked without a server.
#![cfg(feature = "redis")]
use redis::{Value, parse_redis_value};

const COMMAND: &[u8] = include_bytes!("fixtures/redis/valkey-9.0.7-command.resp3");
const DOCS: &[u8] = include_bytes!("fixtures/redis/valkey-9.0.7-command-docs.resp3");

fn p(bytes: &[u8]) -> Value {
    parse_redis_value(bytes).unwrap()
}

#[test]
fn the_fixtures_keep_their_crlf_bytes() {
    assert_eq!((COMMAND.len(), DOCS.len()), (96_888, 234_138));
    assert!(COMMAND.starts_with(b"*254\r\n*10\r\n$4\r\nkeys\r\n:2\r\n~1\r\n+readonly\r\n"));
}

#[test]
fn the_command_reply_is_254_entries_of_ten_fields_with_sets() {
    let Value::Array(entries) = p(COMMAND) else {
        panic!("COMMAND is not an array")
    };
    assert_eq!(entries.len(), 254);
    for e in &entries {
        let Value::Array(f) = e else { panic!("{e:?}") };
        assert_eq!(f.len(), 10, "{e:?}");
        assert!(matches!(f[0], Value::BulkString(_)), "{e:?}");
        assert!(matches!(f[1], Value::Int(_)), "{e:?}");
        for i in [2, 6, 7, 8] {
            assert!(matches!(f[i], Value::Set(_)), "{i}: {e:?}");
        }
        assert!(matches!(f[9], Value::Array(_)), "{e:?}");
    }
}

#[test]
fn the_docs_reply_is_a_map_of_254_maps() {
    let Value::Map(docs) = p(DOCS) else {
        panic!("COMMAND DOCS is not a map")
    };
    assert_eq!(docs.len(), 254);
    assert!(
        docs.iter()
            .all(|(k, v)| matches!(k, Value::BulkString(_)) && matches!(v, Value::Map(_)))
    );
}

#[test]
fn replies_parse_to_the_variants_the_shape_rules_name() {
    assert_eq!(p(b"+OK\r\n"), Value::Okay);
    assert_eq!(p(b"+QUEUED\r\n"), Value::SimpleString("QUEUED".into()));
    // num-bigint stays off, so a big number keeps the server's digits.
    assert_eq!(p(b"(123\r\n"), Value::BigNumber(b"123".to_vec()));
    assert_eq!(p(b"*-1\r\n"), Value::Nil);
    assert_eq!(p(b"#t\r\n"), Value::Boolean(true));
    assert!(matches!(p(b",inf\r\n"), Value::Double(x) if x.is_infinite()));
    assert!(matches!(p(b"=7\r\ntxt:abc\r\n"), Value::VerbatimString { text, .. } if text == "abc"));
    assert!(matches!(
        p(b"|1\r\n+a\r\n:1\r\n:5\r\n"),
        Value::Attribute { data, .. } if *data == Value::Int(5)
    ));
    assert!(matches!(p(b">2\r\n+message\r\n+x\r\n"), Value::Push { .. }));
    let Value::ServerError(e) = p(b"-NOPERM User x has no permissions\r\n") else {
        panic!("not an error")
    };
    assert_eq!(
        (e.code(), e.details()),
        ("NOPERM", Some("User x has no permissions"))
    );
}
