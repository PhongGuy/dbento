//! Its own test binary, so the container under test is the only one this process starts.
// Test helpers may panic on a broken fixture too; clippy's in-tests exemption covers only
// #[test] functions, not the helpers they call.
#![allow(clippy::unwrap_used)]
use dbento_testkit::pg_container;

#[tokio::test]
async fn a_test_container_creates_no_volume() {
    let db = pg_container().await;
    let id = db
        .container_id()
        .expect("pg_container runs on a container")
        .to_owned();
    // Asked of the container itself while it runs, so another process's volumes never count
    // and a run killed before drop leaves nothing either.
    let mounts = dbento_testkit::mounts(&id)
        .await
        .expect("inspecting the container");
    let named: Vec<String> = mounts.iter().filter_map(|m| m.name.clone()).collect();
    let volumes: Vec<_> = mounts.iter().filter(|m| m.kind == "volume").collect();
    assert!(
        volumes.is_empty(),
        "volume mounts while running: {volumes:?}"
    );
    assert_eq!(db.scalar("SELECT 1").await.as_deref(), Some("1"));
    drop(db);
    let all = dbento_testkit::volumes().await.expect("listing volumes");
    let left: Vec<_> = named.iter().filter(|n| all.contains(*n)).collect();
    assert!(left.is_empty(), "volumes left after drop: {left:?}");
}
