//! Its own test binary, so no other test in the process starts a container meanwhile.
use std::collections::BTreeSet;

use dbento_testkit::pg_container;

// Asked of the engine testcontainers itself uses, so Docker on CI and podman locally both count.
async fn volumes() -> BTreeSet<String> {
    dbento_testkit::volumes().await.expect("listing volumes")
}

#[tokio::test]
async fn a_test_container_creates_no_volume() {
    let before = volumes().await;
    let db = pg_container().await;
    // Checked while the container runs, so a run killed before drop leaves nothing either.
    let running: Vec<_> = volumes().await.difference(&before).cloned().collect();
    assert!(running.is_empty(), "new volumes while running: {running:?}");
    assert_eq!(db.scalar("SELECT 1").await.as_deref(), Some("1"));
    drop(db);
    let after: Vec<_> = volumes().await.difference(&before).cloned().collect();
    assert!(after.is_empty(), "new volumes after drop: {after:?}");
}
