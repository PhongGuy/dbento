use std::time::Duration;

use serde_json::json;

use super::*;
use crate::dynamo::replay::{Replay, Reply, aws_error, described, orders, scanned, tables};
use crate::introspect::{IntrospectError, RelationKind, RelationRef};
use crate::session::ExecuteError;

fn orders_keys() -> TableKeys {
    TableKeys {
        hash: "pk".into(),
        range: Some("sk".into()),
        types: [("pk", "S"), ("sk", "N"), ("status", "S"), ("created", "N")]
            .map(|(a, t)| (a.to_owned(), t.to_owned()))
            .into_iter()
            .collect(),
        indexes: vec![
            (
                "by_status".into(),
                "GSI",
                vec!["status".into(), "created".into()],
            ),
            (
                "by_created".into(),
                "LSI",
                vec!["pk".into(), "created".into()],
            ),
        ],
    }
}

#[test]
fn the_only_schema_is_the_region() {
    let names: Vec<String> = schemas("eu-west-1")
        .iter()
        .map(|s| s.name().to_owned())
        .collect();
    assert_eq!(names, ["eu-west-1"]);
}

#[tokio::test]
async fn relations_follow_the_last_table_name_and_sort() {
    let replay = Replay::new(vec![
        Reply::ok(tables(&["b", "a"], Some("b"))),
        Reply::ok(tables(&["c"], None)),
    ]);
    let list = relations(&replay.client(false), "eu-west-1", "eu-west-1")
        .await
        .unwrap();
    let names: Vec<&str> = list.items().iter().map(|r| r.name()).collect();
    assert_eq!(names, ["a", "b", "c"]);
    assert!(list.items().iter().all(|r| r.kind() == RelationKind::Table) && !list.truncated());
    assert_eq!(
        replay.seen()[1].body,
        json!({ "ExclusiveStartTableName": "b" })
    );
    let other = Replay::new(vec![]);
    assert!(
        relations(&other.client(false), "eu-west-1", "us-east-1")
            .await
            .unwrap()
            .items()
            .is_empty()
    );
    assert!(other.seen().is_empty());
}

#[tokio::test]
async fn describe_reads_keys_types_and_indexes_and_a_missing_table_is_none() {
    let replay = Replay::new(vec![
        Reply::ok(described(orders())),
        Reply::error(
            400,
            aws_error("ResourceNotFoundException", "Requested resource not found"),
        ),
    ]);
    let client = replay.client(false);
    let keys = describe(&client, "orders").await.unwrap().unwrap();
    assert_eq!(keys, orders_keys());
    assert_eq!(keys.key_names(), ["pk", "sk"]);
    assert_eq!(describe(&client, "nope").await.unwrap(), None);
}

#[tokio::test]
async fn columns_put_keys_first_then_sampled_attributes_by_count() {
    let replay = Replay::new(vec![Reply::ok(scanned(&[
        json!({ "pk": { "S": "a" }, "sk": { "N": "1" }, "status": { "S": "open" }, "note": { "S": "x" } }),
        json!({ "pk": { "S": "b" }, "sk": { "N": "2" }, "status": { "S": "shut" }, "note": { "N": "3" } }),
        json!({ "pk": { "S": "c" }, "sk": { "N": "3" }, "status": { "NULL": true } }),
    ]))]);
    let got = columns(&replay.client(false), "orders", &orders_keys())
        .await
        .unwrap();
    let shown: Vec<(&str, &str, bool, Option<u16>, bool)> = got
        .iter()
        .map(|c| {
            (
                c.name(),
                c.type_name(),
                c.nullable(),
                c.primary_key(),
                c.inferred(),
            )
        })
        .collect();
    assert_eq!(
        shown,
        [
            ("pk", "S", false, Some(1), false),
            ("sk", "N", false, Some(2), false),
            ("status", "S | NULL", true, None, true),
            ("note", "N | S", true, None, true),
        ]
    );
    assert_eq!(
        replay.seen()[0].body,
        json!({ "TableName": "orders", "Limit": 100 })
    );
}

#[tokio::test]
async fn a_denied_sample_is_an_error() {
    let replay = Replay::new(vec![Reply::error(
        400,
        aws_error("AccessDeniedException", "no Scan"),
    )]);
    match columns(&replay.client(false), "orders", &orders_keys()).await {
        Err(IntrospectError::Execute(ExecuteError::Database { code, .. })) => {
            assert_eq!(code, "AccessDeniedException")
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test(start_paused = true)]
async fn a_slow_sample_stops_at_five_seconds() {
    let replay = Replay::new(vec![Reply::ok(scanned(&[])).after(Duration::from_secs(30))]);
    let got = columns(&replay.client(false), "orders", &orders_keys()).await;
    assert!(
        matches!(got, Err(IntrospectError::Execute(ExecuteError::Timeout))),
        "{got:?}"
    );
}

/// An index as the test compares it: name, columns, method, unique, primary.
type IndexShown<'a> = (&'a str, Vec<&'a str>, Option<&'a str>, bool, bool);

#[test]
fn every_gsi_and_lsi_is_an_index_of_the_table() {
    let rel = RelationRef::new("eu-west-1", "orders");
    let objects = relation_objects(&rel, &orders_keys());
    let shown: Vec<IndexShown<'_>> = objects
        .indexes()
        .iter()
        .map(|i| {
            (
                i.name(),
                i.columns().iter().map(String::as_str).collect(),
                i.method(),
                i.unique(),
                i.primary(),
            )
        })
        .collect();
    assert_eq!(
        shown,
        [
            (
                "by_status",
                vec!["status", "created"],
                Some("GSI"),
                false,
                false
            ),
            (
                "by_created",
                vec!["pk", "created"],
                Some("LSI"),
                false,
                false
            ),
        ]
    );
}

/// `count` ListTables pages of `size` names each, numbered in order; every page but the last
/// names its final table as the place to resume.
fn table_pages(count: usize, size: usize) -> Vec<Reply> {
    (0..count)
        .map(|p| {
            let names: Vec<String> = (p * size..(p + 1) * size)
                .map(|i| format!("t{i:05}"))
                .collect();
            let refs: Vec<&str> = names.iter().map(String::as_str).collect();
            let last = (p + 1 < count).then(|| refs[size - 1]);
            Reply::ok(tables(&refs, last))
        })
        .collect()
}

#[tokio::test]
async fn relations_stop_paging_past_the_cap_and_mark_the_list_truncated() {
    // Four pages cross the cap; the fifth must never be asked for.
    let replay = Replay::new(table_pages(5, 3_000));
    let list = relations(&replay.client(false), "eu-west-1", "eu-west-1")
        .await
        .unwrap();
    assert_eq!(
        replay.seen().len(),
        4,
        "paging stops once the cap is passed"
    );
    assert_eq!(list.items().len(), RELATION_CAP);
    assert!(list.truncated());
    assert_eq!(list.items()[0].name(), "t00000");
    assert_eq!(
        list.items()[RELATION_CAP - 1].name(),
        format!("t{:05}", RELATION_CAP - 1)
    );
}

#[tokio::test]
async fn relations_below_or_at_the_cap_are_whole_and_not_truncated() {
    for (count, size) in [(3, 3_000), (4, RELATION_CAP / 4)] {
        let replay = Replay::new(table_pages(count, size));
        let list = relations(&replay.client(false), "eu-west-1", "eu-west-1")
            .await
            .unwrap();
        assert_eq!(replay.seen().len(), count, "{count} x {size}");
        assert_eq!(list.items().len(), count * size, "{count} x {size}");
        assert!(!list.truncated(), "{count} x {size}");
    }
}
