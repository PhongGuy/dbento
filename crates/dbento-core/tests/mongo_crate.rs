//! The mongodb 3.9.1 and bson 3.1.0 behaviours the MongoDB engine relies on. A failure after a
//! bump means a premise moved: re-plan, do not patch.
#![cfg(feature = "mongodb")]

use bson::spec::BinarySubtype;
use bson::{Binary, Bson, DateTime, Decimal128, Regex, Uuid, doc};
use mongodb::options::ConnectionString;

#[test]
fn the_driver_parser_reports_userinfo_auth_source_and_default_database() {
    let c =
        ConnectionString::parse("mongodb://app@db.internal:27017/shop?authSource=admin").unwrap();
    assert_eq!(c.default_database.as_deref(), Some("shop"));
    let cred = c.credential.unwrap();
    assert_eq!(
        (
            cred.username.as_deref(),
            cred.password.as_deref(),
            cred.source.as_deref()
        ),
        (Some("app"), None, Some("admin"))
    );
    let pw = ConnectionString::parse("mongodb://app:s3cret@h/x").unwrap();
    assert_eq!(pw.credential.unwrap().password.as_deref(), Some("s3cret"));
    let empty = ConnectionString::parse("mongodb://app:@h/x").unwrap();
    assert_eq!(empty.credential.unwrap().password.as_deref(), Some(""));
    let anon = ConnectionString::parse("mongodb://localhost/").unwrap();
    assert!(anon.credential.is_none() && anon.default_database.is_none());
    let enc = ConnectionString::parse("mongodb://app%40corp@h").unwrap();
    assert_eq!(
        enc.credential.unwrap().username.as_deref(),
        Some("app@corp")
    );
    // An SRV string parses without a DNS lookup.
    ConnectionString::parse("mongodb+srv://app@cluster0.example.net/shop").unwrap();
    let bad = ConnectionString::parse("postgres://h/x").unwrap_err();
    assert!(matches!(
        &*bad.kind,
        mongodb::error::ErrorKind::InvalidArgument { message, .. }
            if message.contains("unsupported connection string scheme")
    ));
}

#[test]
fn bson_keeps_insertion_order_and_builds_every_shell_type() {
    let mut d = doc! { "b": 1, "a": 2 };
    d.insert("b", 3);
    assert_eq!(d.keys().collect::<Vec<_>>(), ["b", "a"]);
    assert_eq!(d.get("b"), Some(&Bson::Int32(3)));
    let re = Regex {
        pattern: "a.c".try_into().unwrap(),
        options: "im".try_into().unwrap(),
    };
    assert_eq!(re.to_string(), "/a.c/im");
    assert!(bson::raw::CString::try_from("a\0b").is_err());
    let dec: Decimal128 = "1.10".parse().unwrap();
    assert_eq!(dec.to_string(), "1.10");
    let at = DateTime::parse_rfc3339_str("2024-01-02T03:04:05Z").unwrap();
    assert_eq!(at.timestamp_millis(), 1_704_164_645_000);
    assert!(DateTime::parse_rfc3339_str("2024-01-02").is_err());
    let uuid = Uuid::parse_str("00112233-4455-6677-8899-aabbccddeeff").unwrap();
    assert_eq!(Binary::from_uuid(uuid).subtype, BinarySubtype::Uuid);
    assert_eq!(BinarySubtype::from(0u8), BinarySubtype::Generic);
}
