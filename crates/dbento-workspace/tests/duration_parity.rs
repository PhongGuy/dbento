//! Differential check against humantime 2.4.0 while it is still a dependency.

use std::time::Duration;

use dbento_workspace::{format_duration, parse_duration};

use crate::duration::{ACCEPTED, FORMATTED, REJECTED, sweep};

const HUMANTIME_PANICS: &str = "18446744073709551615s 999999999ns 1ns";

#[test]
fn parser_agrees_with_humantime_on_every_table_input() {
    let inputs = ACCEPTED
        .iter()
        .map(|r| r.0)
        .chain(REJECTED.iter().map(|r| r.0));
    for input in inputs {
        let ours = parse_duration(input).map_err(|e| e.to_string());
        let theirs = humantime::parse_duration(input).map_err(|e| e.to_string());
        assert_eq!(ours, theirs, "input {input:?}");
    }
    assert!(
        std::panic::catch_unwind(|| humantime::parse_duration(HUMANTIME_PANICS)).is_err(),
        "the documented deviation must still be a humantime panic"
    );
}

#[test]
fn formatter_agrees_with_humantime() {
    let table = FORMATTED.iter().map(|&(s, n, _)| Duration::new(s, n));
    for d in table.chain(sweep()) {
        let ours = format_duration(d).to_string();
        assert_eq!(
            ours,
            humantime::format_duration(d).to_string(),
            "duration {d:?}"
        );
        assert_eq!(
            parse_duration(&ours).map_err(|e| e.to_string()),
            humantime::parse_duration(&ours).map_err(|e| e.to_string()),
            "text {ours:?}"
        );
    }
}
