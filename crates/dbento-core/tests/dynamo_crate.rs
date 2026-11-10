//! What Dbento relies on in aws-sdk-dynamodb 1.138.0 and aws-smithy-http-client 1.5.2, checked
//! without a server.
#![cfg(feature = "dynamodb")]
use std::collections::HashMap;

use aws_sdk_dynamodb::config::{BehaviorVersion, Credentials, Region};
use aws_sdk_dynamodb::primitives::Blob;
use aws_sdk_dynamodb::types::AttributeValue;
use aws_smithy_http_client::tls::{self, rustls_provider::CryptoMode};

// Every variant by name: a type the SDK adds later lands in `_`, which clippy then flags.
fn type_name(v: &AttributeValue) -> &'static str {
    match v {
        AttributeValue::S(_) => "S",
        AttributeValue::N(_) => "N",
        AttributeValue::B(_) => "B",
        AttributeValue::Bool(_) => "BOOL",
        AttributeValue::Null(_) => "NULL",
        AttributeValue::M(_) => "M",
        AttributeValue::L(_) => "L",
        AttributeValue::Ss(_) => "SS",
        AttributeValue::Ns(_) => "NS",
        AttributeValue::Bs(_) => "BS",
        AttributeValue::Unknown { .. } => "unknown",
        _ => "unknown",
    }
}

#[test]
fn a_client_builds_on_ring_rustls_with_a_pinned_behaviour_version() {
    let http = aws_smithy_http_client::Builder::new()
        .tls_provider(tls::Provider::Rustls(CryptoMode::Ring))
        .build_https();
    let conf = aws_sdk_dynamodb::Config::builder()
        .behavior_version(BehaviorVersion::v2026_11_10())
        .region(Region::new("eu-west-1"))
        .endpoint_url("http://127.0.0.1:5000")
        .credentials_provider(Credentials::new("AKIDTEST", "secret", None, None, "dbento"))
        .http_client(http)
        .build();
    assert_eq!(conf.region().map(Region::as_ref), Some("eu-west-1"));
    let _client = aws_sdk_dynamodb::Client::from_conf(conf);
}

#[test]
fn every_attribute_type_has_its_variant() {
    let values = [
        AttributeValue::S("a".into()),
        AttributeValue::N("12345678901234567890123456789012345678".into()),
        AttributeValue::B(Blob::new(vec![0u8, 255])),
        AttributeValue::Bool(true),
        AttributeValue::Null(true),
        AttributeValue::M(HashMap::from([(
            "k".to_owned(),
            AttributeValue::N("1".into()),
        )])),
        AttributeValue::L(vec![AttributeValue::Null(true)]),
        AttributeValue::Ss(vec!["x".into()]),
        AttributeValue::Ns(vec!["1".into()]),
        AttributeValue::Bs(vec![Blob::new(vec![1u8])]),
    ];
    let names: Vec<&str> = values.iter().map(type_name).collect();
    assert_eq!(
        names,
        ["S", "N", "B", "BOOL", "NULL", "M", "L", "SS", "NS", "BS"]
    );
    let AttributeValue::B(b) = &values[2] else {
        panic!("not B")
    };
    assert_eq!(b.as_ref(), [0u8, 255]);
}
