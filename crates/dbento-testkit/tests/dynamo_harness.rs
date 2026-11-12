//! The DynamoDB harness on Moto.
#![cfg(feature = "dynamodb")]
use std::collections::BTreeSet;
use std::io::{BufRead, BufReader, Read, Write};
use std::process::{Command, Stdio};

use dbento_testkit::{DySource, ORDERS, REGIONS, RegionLease, dy_source, dynamo_db, lease_from};

/// Set in the child process of the cross-process lease test, to the endpoint it leases on.
const LEASE_CHILD: &str = "DBENTO_TEST_DY_LEASE_CHILD";

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

/// The child half of the cross-process test: holds a lease, prints its region, and keeps the
/// lease until its stdin closes. Without the variable it is a no-op.
#[tokio::test]
async fn lease_child() {
    let Ok(endpoint) = std::env::var(LEASE_CHILD) else {
        return;
    };
    let lease = RegionLease::take(&endpoint).await;
    println!("region={}", lease.region());
    std::io::stdout().flush().unwrap();
    let _ = std::io::stdin().read_to_end(&mut Vec::new());
}

#[tokio::test]
async fn a_region_held_by_another_process_is_skipped_until_it_exits() {
    // An endpoint no other test uses, so only this test and its child contend for it.
    let endpoint = format!("http://lease-test-{}", std::process::id());
    let mut child = Command::new(std::env::current_exe().unwrap())
        .args(["dynamo_harness::lease_child", "--exact", "--nocapture"])
        .env(LEASE_CHILD, &endpoint)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    // Keep reading the child's stdout to the end, so its closing lines never hit a closed pipe.
    let mut out = BufReader::new(child.stdout.take().unwrap()).lines();
    let theirs = out
        .by_ref()
        .map_while(Result::ok)
        .find_map(|l| l.strip_prefix("region=").map(str::to_owned))
        .expect("the child prints its region");
    let mine = RegionLease::take(&endpoint).await;
    assert_ne!(mine.region(), theirs);
    drop(child.stdin.take());
    out.for_each(drop);
    assert!(child.wait().unwrap().success());
    assert_eq!(RegionLease::take(&endpoint).await.region(), theirs);
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
