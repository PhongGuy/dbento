//! The duration syntax `statement_timeout` and `--timeout` accept, pinned to fixed values
//! that humantime 2.4.0 produced for the same inputs, so old workspaces keep parsing.

use std::time::Duration;

use dbento_workspace::{DurationError, format_duration, parse_duration};

/// Input, whole seconds, subsecond nanoseconds.
pub(crate) const ACCEPTED: &[(&str, u64, u32)] = &[
    ("17nsec", 0, 17),
    ("17nanos", 0, 17),
    ("33ns", 0, 33),
    ("3usec", 0, 3000),
    ("78us", 0, 78000),
    ("163µs", 0, 163000),
    ("31msec", 0, 31000000),
    ("31millis", 0, 31000000),
    ("6ms", 0, 6000000),
    ("3000s", 3000, 0),
    ("300sec", 300, 0),
    ("300secs", 300, 0),
    ("50seconds", 50, 0),
    ("1second", 1, 0),
    ("100m", 6000, 0),
    ("12min", 720, 0),
    ("12mins", 720, 0),
    ("1minute", 60, 0),
    ("7minutes", 420, 0),
    ("2h", 7200, 0),
    ("7hr", 25200, 0),
    ("7hrs", 25200, 0),
    ("1hour", 3600, 0),
    ("24hours", 86400, 0),
    ("1day", 86400, 0),
    ("2days", 172800, 0),
    ("365d", 31536000, 0),
    ("1week", 604800, 0),
    ("7weeks", 4233600, 0),
    ("104wks", 62899200, 0),
    ("100wk", 60480000, 0),
    ("52w", 31449600, 0),
    ("1month", 2630016, 0),
    ("3months", 7890048, 0),
    ("12M", 31560192, 0),
    ("1year", 31557600, 0),
    ("7years", 220903200, 0),
    ("15yrs", 473364000, 0),
    ("10yr", 315576000, 0),
    ("17y", 536479200, 0),
    ("3.1usec", 0, 3100),
    ("3.1us", 0, 3100),
    ("3.01us", 0, 3010),
    ("3.001us", 0, 3001),
    ("31.1msec", 0, 31100000),
    ("31.1millis", 0, 31100000),
    ("31.1ms", 0, 31100000),
    ("31.01ms", 0, 31010000),
    ("31.001ms", 0, 31001000),
    ("31.0001ms", 0, 31000100),
    ("31.00001ms", 0, 31000010),
    ("31.000001ms", 0, 31000001),
    ("300.0sec", 300, 0),
    ("300.0secs", 300, 0),
    ("300.0seconds", 300, 0),
    ("300.0s", 300, 0),
    ("0.0s", 0, 0),
    ("0.2s", 0, 200000000),
    ("1.2s", 1, 200000000),
    ("1.02s", 1, 20000000),
    ("1.002s", 1, 2000000),
    ("1.0002s", 1, 200000),
    ("1.00002s", 1, 20000),
    ("1.000002s", 1, 2000),
    ("1.0000002s", 1, 200),
    ("1.00000002s", 1, 20),
    ("1.000000002s", 1, 2),
    ("1.123456789s", 1, 123456789),
    ("100.0m", 6000, 0),
    ("12.1min", 726, 0),
    ("12.1mins", 726, 0),
    ("1.5minute", 90, 0),
    ("1.5minutes", 90, 0),
    ("2.0h", 7200, 0),
    ("2.0hr", 7200, 0),
    ("2.0hrs", 7200, 0),
    ("2.0hours", 7200, 0),
    ("2.5h", 9000, 0),
    ("0.5h", 1800, 0),
    ("1.5day", 129600, 0),
    ("1.5days", 129600, 0),
    ("1.5d", 129600, 0),
    ("7.120us 3ns", 0, 7123),
    ("7.123us 4ns", 0, 7127),
    ("1.234s 789ns", 1, 234000789),
    ("1.234s 0.789us", 1, 234000789),
    ("1.234567s 0.789us", 1, 234567789),
    ("1.234s 1.345ms 1.678us 1ns", 1, 235346679),
    ("1.234s 0.345ms 0.678us 0ns", 1, 234345678),
    ("1.234s0.345ms0.678us0ns", 1, 234345678),
    ("0", 0, 0),
    ("20 min 17 nsec ", 1200, 17),
    ("2h 15m", 8100, 0),
    ("30s", 30, 0),
    (" 30s ", 30, 0),
    ("1m 30s", 90, 0),
    ("1m30s", 90, 0),
    ("30 s", 30, 0),
    ("1h\t2m", 3720, 0),
    ("1h\u{a0}2m", 3720, 0),
    ("5 seconds", 5, 0),
    ("500ms", 0, 500000000),
    ("0s", 0, 0),
    ("2.5 h", 9000, 0),
    ("1h 1h", 7200, 0),
    ("0.5s 0.5s", 1, 0),
    ("18446744073709551615s", 18446744073709551615, 0),
    (
        "18446744073709551615s 999999999ns",
        18446744073709551615,
        999999999,
    ),
    ("1.5w", 907200, 0),
    ("0.5M", 1315008, 0),
    ("0.5y", 15778800, 0),
    ("1.5mins", 90, 0),
    ("1. 5s", 1, 500000000),
    ("1 .5s", 1, 500000000),
    ("1.5 s", 1, 500000000),
    ("2147483647ms", 2147483, 647000000),
    ("2147483648ms", 2147483, 648000000),
    ("24days 20h 31m 23s 647ms", 2147483, 647000000),
    ("1 1s", 11, 0),
];

/// Input and the exact error message.
pub(crate) const REJECTED: &[(&str, &str)] = &[
    ("", "value was empty"),
    ("   ", "value was empty"),
    ("\r", "value was empty"),
    ("\0", "expected number at 0"),
    (".", "expected number at 0"),
    (".1s", "expected number at 0"),
    ("1.s", "invalid character at 1"),
    ("1..s", "invalid character at 1"),
    ("1.", "invalid character at 1"),
    ("1~", "invalid character at 1"),
    ("1Nå", "invalid character at 2"),
    ("123", "time unit needed, for example 123sec or 123ms"),
    ("00", "time unit needed, for example 0sec or 0ms"),
    (" 0", "time unit needed, for example 0sec or 0ms"),
    ("10 months 1", "time unit needed, for example 1sec or 1ms"),
    (
        "10nights",
        "unknown time unit \"nights\", supported units: ns, us/µs, ms, sec, min, hours, days, weeks, months, years (and few variations)",
    ),
    (
        "1S",
        "unknown time unit \"S\", supported units: ns, us/µs, ms, sec, min, hours, days, weeks, months, years (and few variations)",
    ),
    ("1sec2", "time unit needed, for example 2sec or 2ms"),
    ("-1s", "expected number at 0"),
    ("+1s", "expected number at 0"),
    ("1s,2s", "invalid character at 2"),
    ("1.5", "time unit needed, for example 1sec or 1ms"),
    (
        "1e3s",
        "unknown time unit \"e\", supported units: ns, us/µs, ms, sec, min, hours, days, weeks, months, years (and few variations)",
    ),
    ("soon", "expected number at 0"),
    ("2 hours min", "expected number at 8"),
    ("m sec", "expected number at 0"),
    ("1 m sec", "expected number at 4"),
    (
        "222nsec221nanosmsec7s5msec572s",
        "unknown time unit \"nanosmsec\", supported units: ns, us/µs, ms, sec, min, hours, days, weeks, months, years (and few variations)",
    ),
    (
        "0.000123456789s",
        "number is too large or cannot be represented without a lack of precision (values below 1ns are not supported)",
    ),
    (
        "17.5nsec",
        "number is too large or cannot be represented without a lack of precision (values below 1ns are not supported)",
    ),
    (
        "5.1nanos",
        "number is too large or cannot be represented without a lack of precision (values below 1ns are not supported)",
    ),
    (
        "0.0005ns",
        "number is too large or cannot be represented without a lack of precision (values below 1ns are not supported)",
    ),
    (
        "3.0001us",
        "number is too large or cannot be represented without a lack of precision (values below 1ns are not supported)",
    ),
    (
        "0.0001us",
        "number is too large or cannot be represented without a lack of precision (values below 1ns are not supported)",
    ),
    (
        "0.123456us",
        "number is too large or cannot be represented without a lack of precision (values below 1ns are not supported)",
    ),
    (
        "31.0000001ms",
        "number is too large or cannot be represented without a lack of precision (values below 1ns are not supported)",
    ),
    (
        "1.0000000002s",
        "number is too large or cannot be represented without a lack of precision (values below 1ns are not supported)",
    ),
    (
        "0.0000000002s",
        "number is too large or cannot be represented without a lack of precision (values below 1ns are not supported)",
    ),
    (
        "0.00000005d",
        "number is too large or cannot be represented without a lack of precision (values below 1ns are not supported)",
    ),
    (
        "100000000000000000000ns",
        "number is too large or cannot be represented without a lack of precision (values below 1ns are not supported)",
    ),
    (
        "100000000000000000us",
        "number is too large or cannot be represented without a lack of precision (values below 1ns are not supported)",
    ),
    (
        "100000000000000ms",
        "number is too large or cannot be represented without a lack of precision (values below 1ns are not supported)",
    ),
    (
        "100000000000000000000s",
        "number is too large or cannot be represented without a lack of precision (values below 1ns are not supported)",
    ),
    (
        "10000000000000000000m",
        "number is too large or cannot be represented without a lack of precision (values below 1ns are not supported)",
    ),
    (
        "1000000000000000000h",
        "number is too large or cannot be represented without a lack of precision (values below 1ns are not supported)",
    ),
    (
        "100000000000000000d",
        "number is too large or cannot be represented without a lack of precision (values below 1ns are not supported)",
    ),
    (
        "10000000000000000w",
        "number is too large or cannot be represented without a lack of precision (values below 1ns are not supported)",
    ),
    (
        "1000000000000000M",
        "number is too large or cannot be represented without a lack of precision (values below 1ns are not supported)",
    ),
    (
        "10000000000000y",
        "number is too large or cannot be represented without a lack of precision (values below 1ns are not supported)",
    ),
    (
        "18446744073709551615s 1s",
        "number is too large or cannot be represented without a lack of precision (values below 1ns are not supported)",
    ),
    ("1ms!", "invalid character at 3"),
    ("s", "expected number at 0"),
    ("µ", "expected number at 0"),
    (
        "1µ",
        "unknown time unit \"µ\", supported units: ns, us/µs, ms, sec, min, hours, days, weeks, months, years (and few variations)",
    ),
];

/// Whole seconds, subsecond nanoseconds, the formatted text.
pub(crate) const FORMATTED: &[(u64, u32, &str)] = &[
    (0, 0, "0s"),
    (0, 1, "1ns"),
    (0, 123000, "123us"),
    (1, 500000000, "1s 500ms"),
    (5, 0, "5s"),
    (10, 0, "10s"),
    (90, 0, "1m 30s"),
    (3600, 0, "1h"),
    (86400, 0, "1day"),
    (172800, 0, "2days"),
    (31557600, 0, "1year"),
    (2630016, 0, "1month"),
    (2147483, 647000000, "24days 20h 31m 23s 647ms"),
    (2147483, 648000000, "24days 20h 31m 23s 648ms"),
    (34277677, 1001001, "1year 1month 1day 1h 1m 1s 1ms 1us 1ns"),
    (9420, 0, "2h 37m"),
    (0, 32000000, "32ms"),
    (
        18446744073709551615,
        999999999,
        "584542046090years 7months 15days 17h 5m 3s 999ms 999us 999ns",
    ),
    (1, 0, "1s"),
    (60, 0, "1m"),
    (0, 1000, "1us"),
];

/// A deterministic spread of durations up to year 9999 for round-trip checks (xorshift64).
pub(crate) fn sweep() -> Vec<Duration> {
    let mut x: u64 = 0x9E37_79B9_7F4A_7C15;
    let mut out: Vec<Duration> = (0..86_400)
        .step_by(37)
        .map(|s| Duration::new(s, 0))
        .collect();
    for _ in 0..10_000 {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        let secs = x % 253_370_764_800;
        let nanos = u32::try_from((x >> 20) % 1_000_000_000).unwrap();
        out.push(Duration::new(secs, nanos));
        out.push(Duration::new(secs, 0));
    }
    out
}

#[test]
fn every_accepted_form_parses_to_the_same_duration() {
    for &(input, secs, nanos) in ACCEPTED {
        assert_eq!(
            parse_duration(input),
            Ok(Duration::new(secs, nanos)),
            "input {input:?}"
        );
    }
}

#[test]
fn every_rejected_form_gives_the_same_message() {
    for &(input, message) in REJECTED {
        let err = parse_duration(input).expect_err(input);
        assert_eq!(err.to_string(), message, "input {input:?}");
    }
}

#[test]
fn a_carry_past_the_largest_duration_is_an_overflow_not_a_panic() {
    assert_eq!(
        parse_duration("18446744073709551615s 999999999ns 1ns"),
        Err(DurationError::NumberOverflow),
        "humantime panics here; the error is the only sane answer"
    );
}

#[test]
fn formatting_reads_the_same() {
    for &(secs, nanos, text) in FORMATTED {
        assert_eq!(
            format_duration(Duration::new(secs, nanos)).to_string(),
            text,
            "duration {secs}s {nanos}ns"
        );
    }
}

#[test]
fn formatted_text_parses_back_to_the_same_duration() {
    for d in sweep() {
        let text = format_duration(d).to_string();
        assert_eq!(parse_duration(&text), Ok(d), "text {text:?}");
    }
}

#[test]
fn the_error_works_as_a_clap_value_parser_error() {
    fn boxable<E: std::error::Error + Send + Sync + 'static>(_: &E) {}
    boxable(&DurationError::Empty);
}
