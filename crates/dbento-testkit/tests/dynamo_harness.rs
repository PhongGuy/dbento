//! The DynamoDB harness on Moto.
#![cfg(feature = "dynamodb")]
use std::collections::BTreeSet;

use dbento_testkit::{DySource, ORDERS, REGIONS, dy_source, dynamo_db, lease_from};

#[test]
fn a_url_wins_and_anything_else_is_a_container() {
    let url = "http://127.0.0.1:5000";
    assert_eq!(
        dy_source(|k| (k == "DBENTO_DYNAMO_URL").then(|| url.to_owned())),
        DySource::Url(url.to_owned())
    );
    assert_eq!(dy_source(|_| Some(String::new())), DySource::Container);
    assert_eq!(dy_source(|_| None), DySource::Container);
}

#[test]
fn leases_take_the_lowest_free_region_until_none_is_left() {
    let mut taken = BTreeSet::from([0, 2]);
    assert_eq!(lease_from(&mut taken), Some(1));
    assert_eq!(lease_from(&mut taken), Some(3));
    let mut full: BTreeSet<usize> = (0..REGIONS.len()).collect();
    assert_eq!(lease_from(&mut full), None);
}

#[tokio::test]
async fn a_handle_is_seeded_in_its_own_region() {
    let (a, b) = tokio::join!(dynamo_db(), dynamo_db());
    assert_ne!(a.region(), b.region());
    let listed = a.client().list_tables().send().await.unwrap();
    assert_eq!(listed.table_names(), ["h", "orders", "types"]);
    assert_eq!(
        (
            a.count("orders").await,
            a.count("h").await,
            a.count("types").await
        ),
        (ORDERS, 0, 1)
    );
    let toml = a.connection_toml("moto", "read_only = true");
    assert!(
        toml.contains("engine = \"dynamodb\"")
            && toml.contains(a.endpoint())
            && toml.ends_with("read_only = true\n"),
        "{toml}"
    );
}
