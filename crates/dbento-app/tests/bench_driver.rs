use dbento_app::bench_driver::{
    BenchConfig, BenchMode, BenchReport, CancelDriver, CancelStep, Driver, Step, WARM_FRAMES,
    Warmup, end_label,
};
use dbento_app::runner::{StatementEnd, StopReason};
use std::time::{Duration, Instant};

fn at(t0: Instant, ms: u64) -> Instant {
    t0 + Duration::from_millis(ms)
}

#[test]
fn loading_pulls_while_more_waits_otherwise_and_stamps_the_first_paint() {
    let t0 = Instant::now();
    let mut d = Driver::new();
    assert_eq!(d.on_frame(at(t0, 10), 0, false, false), Step::Wait);
    assert_eq!(d.on_frame(at(t0, 26), 200, true, false), Step::Pull);
    assert_eq!(
        d.first_paint(t0),
        None,
        "rows drawn this frame show at the next"
    );
    assert_eq!(d.on_frame(at(t0, 43), 1224, true, false), Step::Pull);
    assert_eq!(d.first_paint(t0), Some(Duration::from_millis(43)));
    assert_eq!(d.on_frame(at(t0, 60), 1500, false, false), Step::Wait);
    assert_eq!(
        d.first_paint(t0),
        Some(Duration::from_millis(43)),
        "stamped once"
    );
}

#[test]
fn loading_finishes_at_the_end_without_scrolling() {
    let t0 = Instant::now();
    let mut d = Driver::new();
    assert_eq!(d.on_frame(at(t0, 10), 200, true, false), Step::Pull);
    assert_eq!(d.on_frame(at(t0, 26), 1224, true, false), Step::Pull);
    assert_eq!(d.on_frame(at(t0, 43), 2248, false, true), Step::Finish);
    assert_eq!(d.first_paint(t0), Some(Duration::from_millis(26)));
    assert_eq!(
        d.on_frame(at(t0, 60), 2248, false, true),
        Step::Finish,
        "finished stays finished"
    );
}

#[test]
fn a_result_ending_with_its_first_rows_waits_one_frame_for_the_paint() {
    let t0 = Instant::now();
    let mut d = Driver::new();
    assert_eq!(d.on_frame(at(t0, 10), 0, false, false), Step::Wait);
    assert_eq!(d.on_frame(at(t0, 26), 500, false, true), Step::Wait);
    assert_eq!(d.first_paint(t0), None);
    assert_eq!(d.on_frame(at(t0, 43), 500, false, true), Step::Finish);
    assert_eq!(d.first_paint(t0), Some(Duration::from_millis(43)));
}

#[test]
fn an_empty_or_failed_run_finishes_at_once() {
    let mut d = Driver::default();
    assert_eq!(d.on_frame(Instant::now(), 0, false, true), Step::Finish);
    assert_eq!(d.first_paint(Instant::now()), None);
}

#[test]
fn the_report_reads_the_wire_form() {
    let wire = r#"{"case":"load_wide_pg","rows":1000000,"heap_bytes":7,"hwm_before_kib":1,"hwm_loaded_kib":2,"hwm_end_kib":3,"first_paint_ms":null,"cancel_ms":12.5,"end":"Complete"}"#;
    let r: BenchReport = serde_json::from_str(wire).unwrap();
    assert_eq!(
        (r.rows, r.cancel_ms, r.hwm_loaded_kib),
        (1_000_000, Some(12.5), 2)
    );
    let json = serde_json::to_string(&r).unwrap();
    for gone in ["intervals_ms", "page_rows"] {
        assert!(!json.contains(gone), "{gone} in {json}");
    }
    let back: BenchReport = serde_json::from_str(&json).unwrap();
    assert_eq!(back, r);
}

#[test]
fn the_config_accepts_load_and_cancel_only() {
    let job = |mode: &str| {
        format!(
            r#"{{"case":"c","mode":"{mode}","connection":"pg","sql":"SELECT 1","out":"/tmp/c.json","max_frames":null}}"#
        )
    };
    let load: BenchConfig = serde_json::from_str(&job("load")).unwrap();
    assert_eq!(load.mode, BenchMode::Load);
    let cancel: BenchConfig = serde_json::from_str(&job("cancel")).unwrap();
    assert_eq!(cancel.mode, BenchMode::Cancel);
    for gone in ["scroll", "calibrate"] {
        assert!(
            serde_json::from_str::<BenchConfig>(&job(gone)).is_err(),
            "{gone} still parses"
        );
    }
}

#[test]
fn ends_read_as_labels() {
    assert_eq!(end_label(&StatementEnd::Skipped), "Skipped");
    let stopped = StatementEnd::Stopped {
        rows: 3,
        reason: StopReason::User,
    };
    assert_eq!(end_label(&stopped), "Stopped: user");
}

#[test]
fn a_cancel_is_timed_from_when_it_was_sent_to_the_running_statement() {
    let t0 = Instant::now();
    let mut c = CancelDriver::new(t0);
    assert_eq!(c.on_frame(at(t0, 10), false, false), CancelStep::Wait);
    assert_eq!(c.on_frame(at(t0, 20), true, false), CancelStep::Wait);
    assert_eq!(c.on_frame(at(t0, 400), true, false), CancelStep::Wait);
    assert_eq!(c.on_frame(at(t0, 520), true, false), CancelStep::Cancel);
    assert_eq!(
        c.on_frame(at(t0, 540), true, false),
        CancelStep::Wait,
        "cancels once"
    );
    assert_eq!(
        c.on_frame(at(t0, 600), true, true),
        CancelStep::Finish(Some(Duration::from_millis(80)))
    );
}

#[test]
fn a_statement_that_ends_before_the_cancel_never_counts_as_cancelled() {
    let t0 = Instant::now();
    let mut c = CancelDriver::new(t0);
    assert_eq!(c.on_frame(at(t0, 20), true, false), CancelStep::Wait);
    assert_eq!(
        c.on_frame(at(t0, 100), true, true),
        CancelStep::Finish(None)
    );
    let mut late = CancelDriver::new(t0);
    assert_eq!(late.on_frame(at(t0, 20), true, false), CancelStep::Wait);
    assert_eq!(
        late.on_frame(at(t0, 700), true, true),
        CancelStep::Finish(None),
        "ended while due"
    );
}

#[test]
fn no_end_within_the_cap_finishes_without_a_cancel_time() {
    let t0 = Instant::now();
    let mut never = CancelDriver::new(t0);
    assert_eq!(
        never.on_frame(at(t0, 10_001), false, false),
        CancelStep::Finish(None)
    );
    let mut stuck = CancelDriver::new(t0);
    assert_eq!(stuck.on_frame(at(t0, 0), true, false), CancelStep::Wait);
    assert_eq!(stuck.on_frame(at(t0, 500), true, false), CancelStep::Cancel);
    assert_eq!(
        stuck.on_frame(at(t0, 10_400), true, false),
        CancelStep::Wait
    );
    assert_eq!(
        stuck.on_frame(at(t0, 10_501), true, false),
        CancelStep::Finish(None)
    );
}

// Feeds `n` warm frames 16 ms apart from `t0` and returns whether any of them fired Run.
fn warm(w: &mut Warmup, t0: Instant, n: usize) -> Vec<bool> {
    (1..=n).map(|i| w.on_frame(at(t0, 16 * i as u64))).collect()
}

#[test]
fn no_run_fires_before_the_warm_frames() {
    let t0 = Instant::now();
    let mut w = Warmup::new(WARM_FRAMES);
    assert!(warm(&mut w, t0, WARM_FRAMES - 1).iter().all(|f| !f));
    assert_eq!(w.run_start(), None);
}

#[test]
fn run_fires_on_exactly_the_warm_up_frame_and_once() {
    let t0 = Instant::now();
    let mut w = Warmup::new(WARM_FRAMES);
    let fired = warm(&mut w, t0, WARM_FRAMES + 5);
    assert_eq!(fired.iter().position(|f| *f), Some(WARM_FRAMES - 1));
    assert_eq!(fired.iter().filter(|f| **f).count(), 1);
    assert_eq!(w.run_start(), Some(at(t0, 16 * WARM_FRAMES as u64)));
}

#[test]
fn first_paint_counts_from_the_run_not_from_window_open() {
    let open = Instant::now();
    let mut w = Warmup::new(WARM_FRAMES);
    warm(&mut w, open, WARM_FRAMES);
    let run = w.run_start().expect("run fired");
    let mut d = Driver::new();
    assert_eq!(
        d.on_frame(run + Duration::from_millis(16), 0, true, false),
        Step::Pull
    );
    assert_eq!(
        d.on_frame(run + Duration::from_millis(33), 50, true, false),
        Step::Pull
    );
    assert_eq!(
        d.on_frame(run + Duration::from_millis(50), 50, true, false),
        Step::Pull
    );
    assert_eq!(d.first_paint(run), Some(Duration::from_millis(50)));
}

#[test]
fn the_cancel_cap_counts_from_the_run() {
    let open = Instant::now();
    let mut w = Warmup::new(WARM_FRAMES);
    warm(&mut w, open, WARM_FRAMES);
    let run = w.run_start().expect("run fired");
    let mut c = CancelDriver::new(run);
    // Past ten seconds from window open but not from Run, the bench still waits.
    let ten = open + Duration::from_secs(10) + Duration::from_millis(100);
    assert!(ten < run + Duration::from_secs(10));
    assert_eq!(c.on_frame(ten, false, false), CancelStep::Wait);
    let past = run + Duration::from_secs(10) + Duration::from_millis(1);
    assert_eq!(c.on_frame(past, false, false), CancelStep::Finish(None));
}

#[test]
fn an_unreported_bench_says_why_it_exits_2() {
    use dbento_app::bench_driver::BenchExit;
    assert_eq!(BenchExit::default(), BenchExit::Unreported);
    assert_eq!(BenchExit::Unreported.code(), 2);
    assert_eq!(
        BenchExit::Unreported.message(),
        Some("event loop ended before the report")
    );
    assert_eq!(
        (BenchExit::Failed.code(), BenchExit::Failed.message()),
        (2, None)
    );
    assert_eq!(
        (BenchExit::Reported.code(), BenchExit::Reported.message()),
        (0, None)
    );
}

#[test]
fn a_log_line_names_the_level_and_target() {
    use dbento_app::bench_driver::log_line;
    assert_eq!(
        log_line(log::Level::Error, "gpui::platform", "broken pipe"),
        "dbento-app bench: ERROR gpui::platform: broken pipe"
    );
}
