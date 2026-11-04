use std::path::Path;
use xtask::app_bench::{AppReport, CASES, Verdict, evaluate, kwin_args, parse_core_median};

fn report(case: &str) -> AppReport {
    serde_json::from_str(&format!(r#"{{"case":"{case}","rows":1000000,"heap_bytes":0,"hwm_before_kib":0,"hwm_loaded_kib":0,"hwm_end_kib":0,"first_paint_ms":null,"cancel_ms":null,"end":"Complete"}}"#)).unwrap()
}
fn passing() -> Vec<AppReport> {
    let mut big = report("load_big_pg");
    big.first_paint_ms = Some(40.0);
    let mut wide = report("load_wide_pg");
    wide.hwm_end_kib = 600 * 1024;
    let mut cancel = report("cancel_pg");
    cancel.cancel_ms = Some(300.0);
    vec![big, wide, cancel]
}
const CORE: [(&str, f64); 2] = [("wide_pg", 571.05), ("wide_sqlite", 470.64)];

fn measures(v: &[Verdict]) -> Vec<(&str, &str)> {
    v.iter().map(|x| (x.case.as_str(), x.measure)).collect()
}

#[test]
fn the_app_report_parses() {
    let r = report("load_wide_pg");
    assert_eq!((r.rows, r.end.as_str()), (1_000_000, "Complete"));
}
#[test]
fn kwin_hosts_the_app_on_a_private_virtual_output() {
    let a = kwin_args(Path::new("/x/dbento-app"), "dbento-bench-1");
    for want in [
        "--virtual",
        "--no-lockscreen",
        "--no-global-shortcuts",
        "--exit-with-session",
    ] {
        assert!(a.iter().any(|s| s == want), "{want} in {a:?}");
    }
    let pair = |k: &str| a.iter().position(|s| s == k).map(|i| a[i + 1].as_str());
    assert_eq!(pair("--socket"), Some("dbento-bench-1"));
    assert_eq!(
        (pair("--width"), pair("--height")),
        (Some("1280"), Some("800"))
    );
    assert_eq!(pair("--exit-with-session"), Some("/x/dbento-app"));
}
#[test]
fn core_medians_come_from_the_bench_report() {
    let out = "dbento bench (release; median of fresh processes)\n\
        wide_pg        median     571.05 MiB/1M min 568.64 max 572.79 runs 3 target < 600 ok\n\
        wide_sqlite    median     470.64 MiB/1M min 470.64 max 470.64 runs 3 target < 600 ok\n";
    assert_eq!(parse_core_median(out, "wide_pg"), Some(571.05));
    assert_eq!(parse_core_median(out, "wide_sqlite"), Some(470.64));
    assert_eq!(parse_core_median(out, "narrow_pg"), None);
}
#[test]
fn evaluate_passes_a_clean_run_with_no_frame_verdicts() {
    let v = evaluate(&passing(), &CORE).unwrap();
    assert!(v.iter().all(|x| x.ok), "{v:?}");
    assert_eq!(
        measures(&v),
        [
            ("load_big_pg", "complete"),
            ("load_big_pg", "first_paint_ms"),
            ("load_wide_pg", "complete"),
            ("load_wide_pg", "growth_mib"),
            ("cancel_pg", "cancel_ms"),
        ]
    );
    let growth = v.iter().find(|x| x.measure == "growth_mib").unwrap();
    assert_eq!(growth.value, 600.0);
    assert!((growth.limit - 628.155).abs() < 1e-9);
}
#[test]
fn evaluate_reports_each_miss() {
    let mut r = passing();
    r[0].first_paint_ms = Some(150.0);
    r[1].hwm_end_kib = 700 * 1024;
    r[1].end = "Stopped: memory budget".into();
    r[2].cancel_ms = Some(1200.0);
    let v = evaluate(&r, &CORE).unwrap();
    let missed: Vec<_> = v
        .iter()
        .filter(|x| !x.ok)
        .map(|x| (x.case.as_str(), x.measure))
        .collect();
    assert_eq!(
        missed,
        [
            ("load_big_pg", "first_paint_ms"),
            ("load_wide_pg", "complete"),
            ("load_wide_pg", "growth_mib"),
            ("cancel_pg", "cancel_ms"),
        ]
    );
}
#[test]
fn a_missing_first_paint_or_cancel_time_is_a_miss() {
    let mut r = passing();
    r[0].first_paint_ms = None;
    r[2].cancel_ms = None;
    let v = evaluate(&r, &CORE).unwrap();
    for (case, measure) in [
        ("load_big_pg", "first_paint_ms"),
        ("cancel_pg", "cancel_ms"),
    ] {
        let x = v
            .iter()
            .find(|x| x.case == case && x.measure == measure)
            .unwrap();
        assert!(x.value.is_nan() && !x.ok, "{x:?}");
    }
}
#[test]
fn a_load_with_no_rows_misses_its_growth() {
    let mut r = passing();
    r[1].rows = 0;
    let v = evaluate(&r, &CORE).unwrap();
    let growth = v.iter().find(|x| x.measure == "growth_mib").unwrap();
    assert!(growth.value.is_nan() && !growth.ok, "{growth:?}");
}
#[test]
fn a_wide_case_without_its_core_figure_is_an_error() {
    let err = evaluate(&passing(), &[]).unwrap_err().to_string();
    assert!(err.contains("wide_pg"), "{err}");
}
#[test]
fn a_big_case_alone_needs_no_baseline() {
    let r = passing();
    let v = evaluate(&r[..1], &[]).unwrap();
    assert!(v.iter().all(|x| x.ok), "{v:?}");
}
#[test]
fn every_case_is_listed_once_with_its_mode_and_connection() {
    let cases: Vec<_> = CASES
        .iter()
        .map(|c| (c.name, c.mode, c.connection))
        .collect();
    assert_eq!(
        cases,
        [
            ("load_big_pg", "load", "pg"),
            ("load_big_sqlite", "load", "lite"),
            ("load_wide_pg", "load", "pg"),
            ("load_wide_sqlite", "load", "lite"),
            ("cancel_pg", "cancel", "pg"),
        ]
    );
}
