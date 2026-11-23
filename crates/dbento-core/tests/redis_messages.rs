//! The message API over Redis Streams: follow, browse both ways, groups and pending.
#![cfg(feature = "redis")]

use std::time::Duration;

use crate::support::{RdFixture, forced, rd};
use dbento_core::SessionOptions;
use dbento_core::message::{MessageError, MessageRead, Position, SourceRef};
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
