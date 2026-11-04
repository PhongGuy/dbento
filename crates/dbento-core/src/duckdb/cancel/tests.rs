use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use super::{DuckCancel, Interrupt, Stop, WorkerGuard};

/// An interrupt that only counts how often it fired.
fn counter() -> (Arc<AtomicUsize>, Interrupt) {
    let n = Arc::new(AtomicUsize::new(0));
    let fired = Arc::clone(&n);
    (
        n,
        Arc::new(move || {
            fired.fetch_add(1, Ordering::SeqCst);
        }),
    )
}

fn guard() -> Arc<DuckCancel> {
    DuckCancel::start(Duration::from_millis(5)).unwrap()
}

fn count(n: &AtomicUsize) -> usize {
    n.load(Ordering::SeqCst)
}

#[test]
fn a_cancel_of_the_running_statement_fires_until_it_ends() {
    let c = guard();
    let (n, interrupt) = counter();
    let g = c.next_generation();
    c.begin(g, None, interrupt).unwrap();
    c.cancel();
    std::thread::sleep(Duration::from_millis(40));
    // DuckDB clears a pending interrupt when execution starts, so one fire is not enough.
    assert!(count(&n) >= 2, "re-fired {} times", count(&n));
    assert_eq!(c.stopped(), Some(Stop::Cancelled));
    assert_eq!(c.end(), Some(Stop::Cancelled));
    let after = count(&n);
    std::thread::sleep(Duration::from_millis(40));
    assert_eq!(count(&n), after, "nothing fires after end");
    c.shut();
}

#[test]
fn a_cancel_before_begin_is_answered_by_begin() {
    let c = guard();
    let (n, interrupt) = counter();
    let g = c.next_generation();
    c.cancel();
    assert_eq!(c.begin(g, None, interrupt), Err(Stop::Cancelled));
    assert_eq!(count(&n), 0);
    assert_eq!(c.stopped(), None);
    c.shut();
}

#[test]
fn a_cancel_between_statements_never_reaches_the_next_one() {
    let c = guard();
    let (n, interrupt) = counter();
    let first = c.next_generation();
    c.begin(first, None, Arc::clone(&interrupt)).unwrap();
    assert_eq!(c.end(), None);
    c.cancel();
    let second = c.next_generation();
    c.begin(second, None, interrupt).unwrap();
    std::thread::sleep(Duration::from_millis(30));
    assert_eq!(count(&n), 0);
    assert_eq!(c.end(), None);
    c.shut();
}

#[test]
fn the_deadline_fires_as_a_timeout() {
    let c = guard();
    let (n, interrupt) = counter();
    let g = c.next_generation();
    let started = Instant::now();
    c.begin(g, Some(started + Duration::from_millis(30)), interrupt)
        .unwrap();
    while c.stopped().is_none() && started.elapsed() < Duration::from_secs(2) {
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(started.elapsed() >= Duration::from_millis(30));
    assert_eq!(c.stopped(), Some(Stop::Timeout));
    assert!(count(&n) >= 1);
    assert_eq!(c.end(), Some(Stop::Timeout));
    c.shut();
}

#[test]
fn whoever_fires_first_names_the_stop() {
    let c = guard();
    let (_, interrupt) = counter();
    let g = c.next_generation();
    c.begin(g, Some(Instant::now()), Arc::clone(&interrupt))
        .unwrap();
    std::thread::sleep(Duration::from_millis(30));
    c.cancel();
    assert_eq!(c.end(), Some(Stop::Timeout), "the deadline fired first");

    let g = c.next_generation();
    c.begin(
        g,
        Some(Instant::now() + Duration::from_millis(20)),
        interrupt,
    )
    .unwrap();
    c.cancel();
    std::thread::sleep(Duration::from_millis(50));
    assert_eq!(c.end(), Some(Stop::Cancelled), "the cancel fired first");
    c.shut();
}

#[test]
fn a_statement_that_ends_before_its_deadline_is_never_interrupted() {
    let c = guard();
    let (n, interrupt) = counter();
    let g = c.next_generation();
    c.begin(
        g,
        Some(Instant::now() + Duration::from_millis(30)),
        interrupt,
    )
    .unwrap();
    assert_eq!(c.end(), None);
    std::thread::sleep(Duration::from_millis(60));
    assert_eq!(count(&n), 0);
    c.shut();
}

#[test]
fn shut_ends_the_watchdog_thread() {
    let c = guard();
    let weak = Arc::downgrade(&c);
    c.shut();
    drop(c);
    let started = Instant::now();
    while weak.upgrade().is_some() {
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the watchdog still holds the guard"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn a_stop_after_execution_started_fires_once_and_rests() {
    // Past execution start DuckDB keeps a pending interrupt for the next fetch, so a parked
    // stream needs one fire, not a re-fire every tick.
    let c = guard();
    let (n, interrupt) = counter();
    let g = c.next_generation();
    c.begin(g, None, interrupt).unwrap();
    c.executed();
    c.cancel();
    std::thread::sleep(Duration::from_millis(40));
    assert_eq!(count(&n), 1);
    assert_eq!(c.end(), Some(Stop::Cancelled));
    c.shut();
}

#[test]
fn a_deadline_past_execution_start_fires_once_and_rests() {
    let c = guard();
    let (n, interrupt) = counter();
    let g = c.next_generation();
    c.begin(
        g,
        Some(Instant::now() + Duration::from_millis(10)),
        interrupt,
    )
    .unwrap();
    c.executed();
    std::thread::sleep(Duration::from_millis(60));
    assert_eq!(c.stopped(), Some(Stop::Timeout));
    assert_eq!(count(&n), 1);
    c.end();
    c.shut();
}

#[test]
fn execution_starting_after_a_stop_fires_once_more_then_rests() {
    // The start of execution clears every earlier fire, so the guard fires again at once.
    let c = guard();
    let (n, interrupt) = counter();
    let g = c.next_generation();
    c.begin(g, None, interrupt).unwrap();
    c.cancel();
    std::thread::sleep(Duration::from_millis(20));
    let before = count(&n);
    c.executed();
    assert_eq!(count(&n), before + 1);
    std::thread::sleep(Duration::from_millis(40));
    assert_eq!(count(&n), before + 1, "no re-fire once execution started");
    c.end();
    c.shut();
}

#[test]
fn a_worker_that_panics_still_ends_its_statement_and_the_watchdog() {
    let c = guard();
    let weak = Arc::downgrade(&c);
    let (n, interrupt) = counter();
    let g = c.next_generation();
    c.begin(g, None, interrupt).unwrap();
    c.cancel();
    // The worker thread panics with a stopped statement that never reported `executed`.
    let held = WorkerGuard::new(Arc::clone(&c));
    let worker = std::thread::spawn(move || {
        let _held = held;
        panic!("the worker died outside a statement's own panic guard");
    });
    assert!(worker.join().is_err());
    let after = count(&n);
    std::thread::sleep(Duration::from_millis(40));
    assert_eq!(count(&n), after, "nothing fires after the worker is gone");
    drop(c);
    let started = Instant::now();
    while weak.upgrade().is_some() {
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the watchdog still holds the guard"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}
