//! The DynamoDB catalog on Moto.
#![cfg(feature = "dynamodb")]
use std::collections::BTreeSet;

use dbento_core::{RelationRef, SessionOptions};

use crate::support::dy;

#[tokio::test]
async fn the_region_is_the_schema_and_lists_the_seeded_tables() {
    let fx = dy("").await;
    let mut s = fx.session(SessionOptions::default()).await;
    let schemas: Vec<String> = s
        .schemas()
        .await
        .unwrap()
        .iter()
        .map(|x| x.name().to_owned())
        .collect();
    assert_eq!(schemas, [fx.db.region()]);
    let tables = s.relations(fx.db.region()).await.unwrap();
    let names: Vec<&str> = tables.items().iter().map(|r| r.name()).collect();
    assert_eq!(names, ["h", "orders", "types"]);
    assert!(s.relations("elsewhere").await.unwrap().items().is_empty());
}

#[tokio::test]
async fn orders_has_keys_first_then_inferred_attributes_and_both_indexes() {
    let fx = dy("").await;
    let mut s = fx.session(SessionOptions::default()).await;
    let rel = RelationRef::new(fx.db.region(), "orders");
    let columns = s.columns(&rel).await.unwrap();
    let keys: Vec<(&str, &str, Option<u16>, bool)> = columns
        .iter()
        .take(2)
        .map(|c| (c.name(), c.type_name(), c.primary_key(), c.inferred()))
        .collect();
    assert_eq!(
        keys,
        [("pk", "S", Some(1), false), ("sk", "N", Some(2), false)]
    );
    let inferred: BTreeSet<&str> = columns
        .iter()
        .skip(2)
        .filter(|c| c.inferred())
        .map(|c| c.name())
        .collect();
    assert_eq!(inferred, BTreeSet::from(["created", "pad", "status"]));
    let objects = s.relation_objects(&rel).await.unwrap();
    let indexes: Vec<(&str, Option<&str>, Vec<&str>)> = objects
        .indexes()
        .iter()
        .map(|i| {
            (
                i.name(),
                i.method(),
                i.columns().iter().map(String::as_str).collect(),
            )
        })
        .collect();
    assert_eq!(
        indexes,
        [
            ("by_status", Some("GSI"), vec!["status", "created"]),
            ("by_created", Some("LSI"), vec!["pk", "created"]),
        ]
    );
}

#[tokio::test]
async fn the_torture_table_infers_all_ten_types() {
    let fx = dy("").await;
    let mut s = fx.session(SessionOptions::default()).await;
    let columns = s
        .columns(&RelationRef::new(fx.db.region(), "types"))
        .await
        .unwrap();
    let types: BTreeSet<&str> = columns
        .iter()
        .filter(|c| c.inferred())
        .map(|c| c.type_name())
        .collect();
    assert_eq!(
        types,
        BTreeSet::from(["B", "BOOL", "BS", "L", "M", "N", "NS", "NULL", "S", "SS"])
    );
}
