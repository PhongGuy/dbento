//! The recent-order list behind Ctrl+Tab: touches, walks and their commit.
use dbento_app::recent_tabs::Recent;

fn list(ids: &[u32]) -> Recent<u32> {
    let mut r = Recent::default();
    for id in ids {
        r.add(*id);
    }
    r
}

#[test]
fn added_tabs_queue_behind_the_touched_ones() {
    let mut r = list(&[1, 2, 3]);
    r.touch(3);
    assert_eq!(r.order(), [3, 1, 2]);
    r.add(4);
    assert_eq!(r.order(), [3, 1, 2, 4]);
}

#[test]
fn a_walk_steps_back_wraps_and_commits_where_it_stopped() {
    let mut r = list(&[1, 2, 3]);
    assert_eq!(r.step(true), Some(2));
    assert_eq!(r.step(true), Some(3));
    assert_eq!(r.step(true), Some(1), "past the oldest the walk wraps");
    assert_eq!(r.step(false), Some(3));
    assert_eq!(
        r.order(),
        [1, 2, 3],
        "a walk reorders nothing until it commits"
    );
    assert!(r.commit());
    assert_eq!(r.order(), [3, 1, 2]);
    assert!(!r.commit(), "no walk, nothing to commit");
}

#[test]
fn a_reversed_walk_starts_at_the_oldest() {
    let mut r = list(&[1, 2, 3]);
    assert_eq!(r.step(false), Some(3));
}

#[test]
fn a_touch_or_a_removal_ends_a_walk() {
    let mut r = list(&[1, 2, 3]);
    r.step(true);
    r.touch(3);
    assert!(!r.walking());
    assert_eq!(r.order(), [3, 1, 2]);
    r.step(true);
    r.remove(2);
    assert!(!r.walking());
    assert_eq!(r.order(), [3, 1]);
}

#[test]
fn an_empty_or_single_list_has_nowhere_to_walk() {
    let mut r: Recent<u32> = Recent::default();
    assert_eq!(r.step(true), None);
    let mut r = list(&[1]);
    assert_eq!(r.step(true), None);
    assert!(!r.walking());
}
