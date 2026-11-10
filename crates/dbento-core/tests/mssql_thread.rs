//! Each SQL Server session has its own connection thread, gone once the session closes.
#![cfg(target_os = "linux")]
// Test helpers may panic on a broken fixture too; clippy's in-tests exemption covers only
// #[test] functions, not the helpers they call.
#![allow(clippy::unwrap_used, clippy::expect_used)]
#![warn(clippy::wildcard_enum_match_arm)]
mod support;

use dbento_core::SessionOptions;
use std::time::{Duration, Instant};

fn connection_threads() -> usize {
    std::fs::read_dir("/proc/self/task")
        .unwrap()
        .filter_map(|t| std::fs::read_to_string(t.ok()?.path().join("comm")).ok())
        .filter(|name| name.trim() == "dbento-mssql")
        .count()
}

#[tokio::test]
async fn every_session_gets_a_thread_of_its_own_that_ends_with_it() {
    dbento_testkit::skip_without_mssql!();
    let fx = support::ms_off().await;
    let a = fx.session(SessionOptions::default()).await;
    let b = fx.session(SessionOptions::default()).await;
    assert_eq!(connection_threads(), 2);
    a.close().await.unwrap();
    drop(b);
    let deadline = Instant::now() + Duration::from_secs(5);
    while connection_threads() > 0 {
        assert!(
            Instant::now() < deadline,
            "{} threads left",
            connection_threads()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}
