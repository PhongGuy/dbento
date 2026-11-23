//! The message API over Redis Streams: follow, browse both ways, groups and pending.
#![cfg(feature = "redis")]

use std::time::{Duration, Instant};

use crate::support::{RdFixture, forced, rd};
use dbento_core::message::{
    MessageCursor, MessageError, MessagePage, MessageRead, Position, SourceRef,
};
use dbento_core::{CellRef, SessionOptions};
use dbento_testkit::RdUser::Reader;

fn src(fx: &RdFixture, key: &str) -> SourceRef {
    SourceRef::redis(fx.db.index(), key.as_bytes().to_vec())
}

fn timed(t: Duration) -> SessionOptions {
    let mut o = forced();
    o.statement_timeout = Some(t);
    o
}

#[tokio::test]
async fn a_wait_over_the_cap_is_refused_before_any_round_trip() {
    let fx = rd("").await;
    // `nosuch` would answer NotFound if anything reached the server.
    for (opts, wait, max) in [
        (
            forced(),
            Duration::from_millis(5_001),
            Duration::from_secs(5),
        ),
        (
            timed(Duration::from_secs(1)),
            Duration::from_millis(751),
            Duration::from_millis(750),
        ),
        (
            timed(Duration::from_millis(200)),
            Duration::from_millis(1),
            Duration::ZERO,
        ),
    ] {
        let mut s = fx.session(Reader, opts).await;
        let r = MessageRead::follow(src(&fx, "nosuch"), Position::End, wait);
        match s.read_messages(&r).await {
            Err(MessageError::WaitTooLong { max: got }) => assert_eq!(got, max),
            other => panic!("{wait:?}: {other:?}"),
        }
    }
}

#[allow(
    clippy::wildcard_enum_match_arm,
    reason = "every cell but text is a failure here"
)]
fn column(p: &MessagePage, c: usize) -> Vec<String> {
    (0..p.batch.len())
        .map(|r| match p.batch.cell(r, c) {
            CellRef::Text(t) => t.to_owned(),
            other => panic!("{other:?}"),
        })
        .collect()
}

fn ids(p: &MessagePage) -> Vec<String> {
    column(p, 0)
}

fn id(c: Option<&MessageCursor>) -> Option<&str> {
    c.and_then(MessageCursor::as_redis_id)
}

fn after(id: &str) -> Position {
    Position::After(MessageCursor::redis_id(id))
}

async fn blocked_clients(fx: &RdFixture, n: u64) {
    for _ in 0..100 {
        let info = fx.db.text(&["INFO", "clients"]).await.unwrap();
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
async fn follow_returns_what_arrives_during_the_wait() {
    let fx = rd("").await;
    let mut s = fx.session(Reader, forced()).await;
    let r = MessageRead::follow(src(&fx, "stream"), after("1-2"), Duration::from_secs(3));
    let began = Instant::now();
    let (page, ()) = tokio::join!(s.read_messages(&r), async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        fx.db.cmd(&["XADD", "stream", "5-1", "h", "x"]).await;
    });
    let page = page.unwrap();
    assert!(
        began.elapsed() < Duration::from_secs(2),
        "{:?}",
        began.elapsed()
    );
    let names: Vec<_> = page.columns.iter().map(|c| c.name()).collect();
    assert_eq!(names, ["id", "time", "fields"]);
    assert_eq!(
        (ids(&page), column(&page, 1)),
        (vec!["5-1".to_owned()], vec!["5".to_owned()])
    );
    assert_eq!(column(&page, 2), [r#"{"h":"x"}"#]);
    assert_eq!(
        (id(page.next.as_ref()), id(page.prev.as_ref())),
        (Some("5-1"), Some("5-1"))
    );
    assert_eq!(
        (page.length, page.gap, page.truncated_by),
        (Some(3), false, None)
    );
}

#[tokio::test]
async fn a_quiet_follow_waits_out_and_keeps_its_cursor() {
    let fx = rd("").await;
    let mut s = fx.session(Reader, forced()).await;
    let r = MessageRead::follow(src(&fx, "stream"), after("1-2"), Duration::from_millis(300));
    let began = Instant::now();
    let page = s.read_messages(&r).await.unwrap();
    let took = began.elapsed();
    assert!(
        took >= Duration::from_millis(250) && took < Duration::from_secs(2),
        "{took:?}"
    );
    assert!(ids(&page).is_empty());
    assert_eq!(
        (id(page.next.as_ref()), id(page.prev.as_ref())),
        (Some("1-2"), None)
    );
    assert_eq!(page.length, Some(2));
}

#[tokio::test]
async fn follow_from_end_skips_what_was_there() {
    let fx = rd("").await;
    let mut s = fx.session(Reader, forced()).await;
    let r = MessageRead::follow(src(&fx, "stream"), Position::End, Duration::from_secs(3));
    let (page, ()) = tokio::join!(s.read_messages(&r), async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        fx.db.cmd(&["XADD", "stream", "5-1", "h", "x"]).await;
    });
    assert_eq!(ids(&page.unwrap()), ["5-1"]);
}

#[tokio::test]
async fn an_empty_stream_follows_from_zero() {
    let fx = rd("").await;
    fx.db
        .cmd(&["XGROUP", "CREATE", "fresh", "g", "$", "MKSTREAM"])
        .await;
    let mut s = fx.session(Reader, forced()).await;
    let quiet = MessageRead::follow(src(&fx, "fresh"), Position::End, Duration::from_millis(50));
    let page = s.read_messages(&quiet).await.unwrap();
    assert_eq!(
        (id(page.next.as_ref()), page.length),
        (Some("0-0"), Some(0))
    );
    let r = MessageRead::follow(src(&fx, "fresh"), Position::End, Duration::from_secs(3));
    let (page, ()) = tokio::join!(s.read_messages(&r), async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        fx.db.cmd(&["XADD", "fresh", "1-1", "a", "b"]).await;
    });
    assert_eq!(ids(&page.unwrap()), ["1-1"]);
}

#[tokio::test]
async fn a_missing_or_deleted_key_ends_the_follow() {
    let fx = rd("").await;
    let mut s = fx.session(Reader, forced()).await;
    let r = MessageRead::follow(src(&fx, "nosuch"), Position::End, Duration::from_secs(4));
    let began = Instant::now();
    assert!(matches!(
        s.read_messages(&r).await,
        Err(MessageError::NotFound)
    ));
    assert!(
        began.elapsed() < Duration::from_secs(1),
        "{:?}",
        began.elapsed()
    );
    let r = MessageRead::follow(src(&fx, "stream"), after("1-2"), Duration::from_millis(100));
    assert!(ids(&s.read_messages(&r).await.unwrap()).is_empty());
    fx.db.cmd(&["DEL", "stream"]).await;
    assert!(matches!(
        s.read_messages(&r).await,
        Err(MessageError::NotFound)
    ));
}

#[tokio::test]
async fn a_one_nanosecond_wait_still_answers() {
    let fx = rd("").await;
    let mut s = fx.session(Reader, forced()).await;
    let r = MessageRead::follow(src(&fx, "stream"), after("1-2"), Duration::from_nanos(1));
    let page = tokio::time::timeout(Duration::from_secs(2), s.read_messages(&r))
        .await
        .expect("BLOCK 0 would wait forever")
        .unwrap();
    assert!(ids(&page).is_empty());
}

#[tokio::test]
async fn a_cancel_during_follow_ends_the_session() {
    let fx = rd("").await;
    let mut s = fx.session(Reader, forced()).await;
    let handle = s.cancel_handle();
    let stop = tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(300)).await;
        handle.cancel();
    });
    let r = MessageRead::follow(src(&fx, "stream"), after("1-2"), Duration::from_secs(3));
    assert!(matches!(
        s.read_messages(&r).await,
        Err(MessageError::Cancelled)
    ));
    stop.await.unwrap();
    assert!(matches!(
        s.read_messages(&r).await,
        Err(MessageError::SessionClosed)
    ));
    blocked_clients(&fx, 0).await;
}

#[tokio::test]
async fn max_bytes_holds_a_follow_back_at_the_cut() {
    let fx = rd("").await;
    let big = "x".repeat(4096);
    for entry in ["5-1", "5-2", "5-3"] {
        fx.db.cmd(&["XADD", "stream", entry, "h", &big]).await;
    }
    let mut s = fx.session(Reader, forced()).await;
    let mut r = MessageRead::follow(src(&fx, "stream"), after("1-2"), Duration::from_millis(100));
    r.max_bytes = Some(6000);
    let page = s.read_messages(&r).await.unwrap();
    assert_eq!(ids(&page), ["5-1"]);
    assert_eq!(page.truncated_by, Some(dbento_core::TruncatedBy::Bytes));
    assert_eq!(id(page.next.as_ref()), Some("5-1"));
    r.from = Position::After(page.next.unwrap());
    assert_eq!(ids(&s.read_messages(&r).await.unwrap()), ["5-2"]);
}
