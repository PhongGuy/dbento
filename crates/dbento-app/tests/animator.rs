//! The grid's wheel animator: eased, clamped, retargetable and frame-rate independent.
use std::time::{Duration, Instant};

use dbento_app::smooth_scroll::Animator;

const MAX: f32 = 10_000.0;

fn ms(n: u64) -> Duration {
    Duration::from_millis(n)
}

#[test]
fn a_new_animator_is_settled_at_the_top() {
    let a = Animator::default();
    assert!(a.is_settled());
    assert_eq!((a.offset(), a.target()), (0.0, 0.0));
}

#[test]
fn a_notch_moves_the_target_at_once_and_the_offset_over_100_ms() {
    let t0 = Instant::now();
    let mut a = Animator::default();
    assert!(a.scroll_by(-78.0, MAX, t0));
    assert_eq!(a.target(), -78.0);
    assert_eq!(a.step(t0), 0.0);
    let mid = a.step(t0 + ms(50));
    assert!(mid < 0.0 && mid > -78.0, "{mid}");
    assert!(!a.is_settled());
    assert_eq!(a.step(t0 + Animator::DURATION), -78.0);
    assert!(a.is_settled());
    assert_eq!(a.step(t0 + ms(500)), -78.0);
}

#[test]
fn the_motion_eases_out() {
    let t0 = Instant::now();
    let mut a = Animator::default();
    a.scroll_by(-100.0, MAX, t0);
    let half = a.step(t0 + ms(50));
    assert!(
        half < -50.0,
        "more than half the way by half the time: {half}"
    );
}

#[test]
fn the_offset_never_passes_the_target_and_never_moves_back() {
    let t0 = Instant::now();
    let mut a = Animator::default();
    a.scroll_by(-200.0, MAX, t0);
    let mut last = 0.0;
    for i in 0..=120 {
        let y = a.step(t0 + ms(i));
        assert!(y <= last && y >= -200.0, "{i} ms: {y} after {last}");
        last = y;
    }
    assert_eq!(last, -200.0);
}

#[test]
fn the_target_stays_within_the_content() {
    let t0 = Instant::now();
    let mut a = Animator::default();
    assert!(a.scroll_by(-500.0, 300.0, t0));
    assert_eq!(a.target(), -300.0);
    assert_eq!(a.step(t0 + ms(100)), -300.0);
    assert!(
        !a.scroll_by(-78.0, 300.0, t0 + ms(110)),
        "at the bottom a notch down is not consumed"
    );
    assert_eq!(a.target(), -300.0);
}

#[test]
fn a_notch_up_at_the_top_is_not_consumed() {
    let mut a = Animator::default();
    assert!(!a.scroll_by(78.0, MAX, Instant::now()));
    assert!(a.is_settled());
}

#[test]
fn content_without_overflow_never_consumes() {
    let mut a = Animator::default();
    assert!(!a.scroll_by(-78.0, 0.0, Instant::now()));
    assert!(a.is_settled());
}

#[test]
fn a_notch_toward_an_edge_still_being_reached_is_consumed() {
    let t0 = Instant::now();
    let mut a = Animator::default();
    a.scroll_by(-300.0, 300.0, t0);
    a.step(t0 + ms(20));
    assert!(a.scroll_by(-78.0, 300.0, t0 + ms(30)));
    assert_eq!(a.target(), -300.0);
    assert_eq!(
        a.step(t0 + ms(100)),
        -300.0,
        "the consumed notch did not restart the motion"
    );
}

#[test]
fn a_second_notch_mid_motion_continues_from_the_shown_offset() {
    let t0 = Instant::now();
    let mut a = Animator::default();
    a.scroll_by(-78.0, MAX, t0);
    a.step(t0 + ms(30));
    let mut reference = a.clone();
    let at40 = reference.step(t0 + ms(40));
    assert!(a.scroll_by(-78.0, MAX, t0 + ms(40)));
    assert_eq!(a.target(), -156.0);
    assert_eq!(a.step(t0 + ms(40)), at40, "no jump when retargeted");
    assert!(
        a.step(t0 + ms(100)) > -156.0,
        "the second notch runs its own 100 ms"
    );
    assert_eq!(a.step(t0 + ms(140)), -156.0);
    assert!(a.is_settled());
}

/// An animator given notches at 0 and 30 ms and stepped every `period` until `until`.
fn driven(t0: Instant, period: Duration, until: Duration) -> Animator {
    let mut a = Animator::default();
    a.scroll_by(-234.0, MAX, t0);
    let (mut t, mut second) = (period, false);
    while t < until {
        if !second && t >= ms(30) {
            a.scroll_by(-100.0, MAX, t0 + ms(30));
            second = true;
        }
        a.step(t0 + t);
        t += period;
    }
    a
}

#[test]
fn the_motion_depends_on_time_not_on_the_frame_rate() {
    let t0 = Instant::now();
    let mut slow = driven(t0, Duration::from_micros(16_667), ms(50));
    let mut fast = driven(t0, Duration::from_micros(6_944), ms(50));
    let (s, f) = (slow.step(t0 + ms(50)), fast.step(t0 + ms(50)));
    assert!((s - f).abs() < 1e-3, "60 Hz {s} against 144 Hz {f}");
    assert!(s < 0.0 && s > -334.0, "{s}");
    assert_eq!(slow.step(t0 + ms(130)), -334.0);
    assert_eq!(fast.step(t0 + ms(130)), -334.0);
}

#[test]
fn a_foreign_offset_change_stops_the_animation_there() {
    let t0 = Instant::now();
    let mut a = Animator::default();
    a.scroll_by(-78.0, MAX, t0);
    let shown = a.step(t0 + ms(20));
    a.sync(shown);
    assert!(
        !a.is_settled(),
        "the handle still holds what the animator wrote"
    );
    a.sync(-5_000.0);
    assert!(a.is_settled());
    assert_eq!((a.offset(), a.target()), (-5_000.0, -5_000.0));
    assert_eq!(a.step(t0 + ms(40)), -5_000.0);
}

#[test]
fn a_settled_animator_follows_the_handle() {
    let mut a = Animator::default();
    a.sync(-420.0);
    assert_eq!((a.offset(), a.target()), (-420.0, -420.0));
    let t0 = Instant::now();
    assert!(a.scroll_by(-78.0, MAX, t0));
    assert_eq!(a.step(t0 + ms(100)), -498.0);
}

#[test]
fn an_overscroll_left_by_another_scroller_is_clamped_before_the_notch() {
    let mut a = Animator::default();
    a.sync(40.0);
    let t0 = Instant::now();
    assert!(a.scroll_by(-78.0, MAX, t0));
    assert_eq!(a.target(), -78.0);
    assert_eq!(a.step(t0), 0.0);
}

#[test]
fn cancel_stops_where_the_offset_stands() {
    let t0 = Instant::now();
    let mut a = Animator::default();
    a.scroll_by(-78.0, MAX, t0);
    let shown = a.step(t0 + ms(20));
    a.cancel();
    assert!(a.is_settled());
    assert_eq!((a.offset(), a.target()), (shown, shown));
}
