//! A stream tab's read state: which read goes next, Follow, the wait cap and panel texts.
use std::time::Duration;

use dbento_app::message_view::{
    FOLLOW_WAIT, Follow, MESSAGE_PAGE, MessageView, PAGE_BYTES, dropped_text, group_text,
    pending_text,
};
use dbento_core::message::{Direction, MessageCursor, Position};

#[test]
fn the_first_read_asks_for_the_newest_page() {
    let v = MessageView::new(3, b"s".to_vec());
    let r = v.newest_read();
    assert!(matches!(r.from, Position::End) && matches!(r.direction, Direction::Backward));
    assert_eq!(
        (r.want, r.max_bytes, r.wait),
        (MESSAGE_PAGE, Some(PAGE_BYTES), None)
    );
    assert_eq!(
        (r.source.db, r.source.name.as_slice()),
        (Some(3), b"s".as_slice())
    );
    assert!(v.older_read().is_none() && !v.has_older());
}

#[test]
fn a_short_backward_page_is_the_last_and_an_empty_one_keeps_newest() {
    let mut v = MessageView::new(0, b"s".to_vec());
    v.begin_read();
    v.browsed(
        Some(MessageCursor::redis_id("1-600")),
        Some(MessageCursor::redis_id("1-101")),
        true,
        false,
    );
    assert!(v.has_older());
    let r = v.older_read().unwrap();
    assert_eq!(r.from, Position::Before(MessageCursor::redis_id("1-101")));
    v.begin_read();
    v.browsed(None, None, false, true);
    assert!(!v.has_older() && v.older_read().is_none());
    assert_eq!(
        v.newest(),
        Some(&MessageCursor::redis_id("1-600")),
        "an empty page keeps newest"
    );
    v.set_follow(Follow::On);
    assert_eq!(
        v.poll_read().unwrap().from,
        Position::After(MessageCursor::redis_id("1-600"))
    );
}

#[test]
fn follow_polls_only_when_on_and_nothing_is_in_flight() {
    let mut v = MessageView::new(0, b"s".to_vec());
    assert!(v.poll_read().is_none(), "Follow starts off");
    v.set_follow(Follow::On);
    let r = v.poll_read().unwrap();
    assert!(matches!(r.from, Position::End) && matches!(r.direction, Direction::Forward));
    assert_eq!((r.wait, r.max_bytes), (Some(FOLLOW_WAIT), Some(PAGE_BYTES)));
    v.begin_poll();
    assert!(v.polling() && v.poll_read().is_none());
    v.set_follow(Follow::Paused);
    v.poll_dropped();
    assert!(
        !v.polling() && v.poll_read().is_none(),
        "a paused view sends nothing"
    );
    v.set_follow(Follow::On);
    assert!(v.poll_read().is_some());
    v.begin_read();
    assert!(v.poll_read().is_none(), "a browse in flight holds the poll");
    v.fail("boom".into());
    assert_eq!(v.error(), Some("boom"));
    assert!(!v.reading() && v.follow() == Follow::On);
}

#[test]
fn a_wait_over_the_cap_shrinks_once_and_a_zero_cap_stops() {
    let mut v = MessageView::new(0, b"s".to_vec());
    v.set_follow(Follow::On);
    assert!(v.wait_too_long(Duration::from_millis(750)));
    assert_eq!(
        v.poll_read().unwrap().wait,
        Some(Duration::from_millis(750))
    );
    assert!(!v.wait_too_long(Duration::ZERO));
    assert_eq!(v.follow(), Follow::Off);
}

#[test]
fn stop_and_drops_end_follow_and_paging_back() {
    let mut v = MessageView::new(0, b"s".to_vec());
    v.set_follow(Follow::On);
    v.begin_poll();
    v.stop(Some("the stream no longer exists".into()));
    assert_eq!(v.follow(), Follow::Off);
    assert!(!v.polling() && !v.reading());
    assert_eq!(v.error(), Some("the stream no longer exists"));
    v.browsed(None, Some(MessageCursor::redis_id("1-1")), false, false);
    v.note_dropped(2);
    v.note_dropped(3);
    assert_eq!(v.dropped(), 5);
    assert!(!v.has_older());
}

#[test]
fn panel_texts_name_counts_in_their_units() {
    assert_eq!(
        group_text("g", 1, 2, Some(0)),
        "g \u{B7} 1 consumer \u{B7} 2 pending \u{B7} lag 0"
    );
    assert_eq!(
        group_text("h", 2, 0, None),
        "h \u{B7} 2 consumers \u{B7} 0 pending"
    );
    assert_eq!(
        pending_text("1-1", "c1", 1234, 1),
        "1-1 \u{B7} c1 \u{B7} idle 1234 ms \u{B7} 1 delivery"
    );
    assert_eq!(
        pending_text("1-2", "c2", 0, 2),
        "1-2 \u{B7} c2 \u{B7} idle 0 ms \u{B7} 2 deliveries"
    );
    assert_eq!(
        (dropped_text(1), dropped_text(5)),
        ("1 older dropped".to_owned(), "5 older dropped".to_owned())
    );
}
