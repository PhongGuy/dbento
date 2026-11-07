//! Timeouts and cancels end the session; SELECT and MULTI are tracked.
#![cfg(feature = "redis")]
mod support;

use std::time::Duration;

use dbento_core::{ExecuteError, SessionOptions};
use dbento_testkit::{
    RdUser::{Reader, Writer},
    RedisDb,
};
use support::{client_field, failure, rd, read};

/// Waits until the server holds `n` blocked clients.
async fn blocked_clients(db: &RedisDb, n: u64) {
    for _ in 0..100 {
        let info = db.text(&["INFO", "clients"]).await.unwrap();
        let now = info
            .lines()
            .find_map(|l| l.strip_prefix("blocked_clients:"))
            .unwrap()
            .trim()
            .parse::<u64>()
            .unwrap();
        if now == n {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("blocked clients never reached {n}");
}

#[tokio::test]
async fn a_timeout_closes_the_connection_and_ends_the_session() {
    let fx = rd("").await;
    let mut opts = SessionOptions::default();
    opts.statement_timeout = Some(Duration::from_millis(50));
    let mut s = fx.session(Writer, opts).await;
    let e = failure(&mut s, "BLPOP nosuch 0").await;
    assert!(matches!(e, ExecuteError::Timeout), "{e:?}");
    let next = failure(&mut s, "GET str").await;
    assert!(next.closes_session(), "{next:?}");
    tokio::time::timeout(Duration::from_secs(1), s.closed())
        .await
        .expect("closed() resolves");
    blocked_clients(&fx.db, 0).await;
}

#[tokio::test]
async fn a_cancel_closes_the_connection_too() {
    let fx = rd("").await;
    let mut s = fx.session(Writer, SessionOptions::default()).await;
    let handle = s.cancel_handle();
    let stop = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        handle.cancel();
    });
    let e = failure(&mut s, "BLPOP nosuch 0").await;
    assert!(matches!(e, ExecuteError::Cancelled), "{e:?}");
    stop.await.unwrap();
    assert!(failure(&mut s, "GET str").await.closes_session());
    blocked_clients(&fx.db, 0).await;
}

#[tokio::test]
async fn select_moves_the_session_and_counts_as_a_settings_change() {
    let fx = rd("").await;
    let mut s = fx.session(Reader, SessionOptions::default()).await;
    assert!(!s.settings_changed());
    read(&mut s, &format!("SELECT {}", fx.other_db())).await;
    assert!(s.settings_changed());
    assert_eq!(client_field(&mut s, "db").await, fx.other_db().to_string());
}

#[tokio::test]
async fn a_select_queued_in_multi_counts_at_exec() {
    let fx = rd("").await;
    let mut s = fx.session(Writer, SessionOptions::default()).await;
    read(&mut s, "MULTI").await;
    read(&mut s, &format!("SELECT {}", fx.other_db())).await;
    assert!(!s.settings_changed());
    read(&mut s, "EXEC").await;
    assert!(s.settings_changed());
    assert_eq!(client_field(&mut s, "db").await, fx.other_db().to_string());
}
