use std::time::Duration;

use super::{
    Direction, MessageCursor, MessageError, MessageRead, Position, ReadPlan, SourceRef, block_ms,
    plan_read,
};

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

fn too_long(r: Result<u64, MessageError>) -> Duration {
    match r {
        Err(MessageError::WaitTooLong { max }) => max,
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_wait_rounds_up_to_whole_milliseconds_and_never_to_zero() {
    assert_eq!(block_ms(Duration::ZERO, None).unwrap(), 1);
    assert_eq!(block_ms(Duration::from_nanos(1), None).unwrap(), 1);
    assert_eq!(block_ms(Duration::from_micros(1_500), None).unwrap(), 2);
    assert_eq!(block_ms(Duration::from_secs(5), None).unwrap(), 5_000);
}

#[test]
fn the_cap_is_five_seconds_or_the_timeout_less_a_margin() {
    assert_eq!(
        too_long(block_ms(ms(5_000) + Duration::from_nanos(1), None)),
        ms(5_000)
    );
    assert_eq!(block_ms(ms(750), Some(ms(1_000))).unwrap(), 750);
    assert_eq!(too_long(block_ms(ms(751), Some(ms(1_000)))), ms(750));
    assert_eq!(too_long(block_ms(ms(6_000), Some(ms(60_000)))), ms(5_000));
    assert_eq!(too_long(block_ms(ms(1), Some(ms(250)))), Duration::ZERO);
    assert_eq!(too_long(block_ms(ms(1), Some(ms(200)))), Duration::ZERO);
}

fn src() -> SourceRef {
    SourceRef::redis(0, b"s".to_vec())
}

fn at(id: &str) -> MessageCursor {
    MessageCursor::redis_id(id)
}

#[test]
fn want_is_clamped_and_browse_pairs_are_checked() {
    let mut r = MessageRead::browse(src(), Position::Start, Direction::Forward);
    r.want = 0;
    assert!(matches!(
        plan_read(&r, None),
        Ok(ReadPlan::Browse { want: 1 })
    ));
    r.want = 50_000;
    assert!(matches!(
        plan_read(&r, None),
        Ok(ReadPlan::Browse { want: 10_000 })
    ));
    for (from, dir) in [
        (Position::Before(at("1-1")), Direction::Forward),
        (Position::After(at("1-1")), Direction::Backward),
    ] {
        let r = MessageRead::browse(src(), from, dir);
        assert!(matches!(
            plan_read(&r, None),
            Err(MessageError::Unsupported { .. })
        ));
    }
}

#[test]
fn follow_reads_forward_from_after_or_end_only() {
    let ok = MessageRead::follow(src(), Position::After(at("1-1")), ms(10));
    assert!(matches!(
        plan_read(&ok, None),
        Ok(ReadPlan::Follow {
            want: 100,
            block_ms: 10
        })
    ));
    let mut backward = MessageRead::follow(src(), Position::End, ms(10));
    backward.direction = Direction::Backward;
    for r in [
        MessageRead::follow(src(), Position::Start, ms(10)),
        MessageRead::follow(src(), Position::Before(at("1-1")), ms(10)),
        backward,
    ] {
        assert!(
            matches!(plan_read(&r, None), Err(MessageError::Unsupported { .. })),
            "{r:?}"
        );
    }
}

#[test]
fn a_redis_cursor_is_its_id() {
    assert_eq!(at("1-2").as_redis_id(), Some("1-2"));
}
