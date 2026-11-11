//! What the shells need from core for DynamoDB: key columns alone and a capacity value.
#[cfg(feature = "dynamodb")]
use dbento_core::SessionOptions;
use dbento_core::{CapacityUnits, IntrospectError, RelationRef};

use crate::support;

#[test]
fn capacity_from_thousandths_reads_as_units() {
    let c = CapacityUnits::from_thousandths(1_500);
    assert_eq!((c.to_string(), c.as_f64()), ("1.5".to_owned(), 1.5));
    assert_eq!(CapacityUnits::from_thousandths(3_000).to_string(), "3");
}

#[cfg(feature = "dynamodb")]
#[tokio::test]
async fn key_columns_on_moto_are_the_declared_keys_alone() {
    let fx = support::dy("").await;
    let mut s = fx.session(SessionOptions::default()).await;
    let keys = s
        .key_columns(&RelationRef::new(fx.db.region(), "orders"))
        .await
        .unwrap();
    let got: Vec<(&str, &str, Option<u16>)> = keys
        .iter()
        .map(|c| (c.name(), c.type_name(), c.primary_key()))
        .collect();
    assert_eq!(got, [("pk", "S", Some(1)), ("sk", "N", Some(2))]);
    assert!(
        s.key_columns(&RelationRef::new(fx.db.region(), "nope"))
            .await
            .unwrap()
            .is_empty()
    );
}

#[tokio::test]
async fn key_columns_are_unsupported_elsewhere() {
    let fx = support::sqlite_fixture("CREATE TABLE t (a PRIMARY KEY)", "");
    let mut s = fx.session(Default::default()).await;
    let got = s.key_columns(&RelationRef::new("main", "t")).await;
    assert!(
        matches!(
            got,
            Err(IntrospectError::Unsupported {
                what: "key columns"
            })
        ),
        "{got:?}"
    );
}
