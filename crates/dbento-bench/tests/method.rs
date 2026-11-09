use dbento_bench::measure::run_child;
use std::path::Path;

fn exe() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_dbento-bench"))
}

#[cfg(target_os = "linux")]
#[test]
fn a_child_reports_the_memory_it_touched() {
    // The slack covers only the kernel's per-CPU counter lag; code paged in mid-run must not count.
    for mib in [0u64, 16, 64] {
        let s = run_child(exe(), &["probe".into(), mib.to_string()], &[]).unwrap();
        assert_eq!((s.case.as_str(), s.rows), ("probe", 0));
        let mib = mib as f64;
        assert!(
            (mib - 0.5..mib + 0.5).contains(&s.value),
            "{mib} MiB probe reported {}",
            s.value
        );
    }
}

#[test]
fn a_failing_child_is_an_error_carrying_its_stderr() {
    let err = run_child(exe(), &["probe".into(), "notanumber".into()], &[]).unwrap_err();
    assert!(format!("{err:#}").contains("notanumber"), "{err:#}");
}
