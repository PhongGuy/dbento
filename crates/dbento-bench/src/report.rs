//! The spec's targets, the row-count guard and the stable report text.

use crate::cases::Case;
use crate::measure::median;
use anyhow::{Result, bail};

/// The unit a case's value is reported in.
pub fn unit(case: Case) -> &'static str {
    match case {
        Case::TtfrPg | Case::TtfrSqlite | Case::StreamPg | Case::StreamSqlite => "ms",
        _ => "MiB/1M",
    }
}

/// The spec's limit for a case; a median must be strictly below it.
pub fn target(case: Case) -> f64 {
    match case {
        Case::TtfrPg => 5.0,
        Case::TtfrSqlite => 1.0,
        Case::StreamPg | Case::StreamSqlite => 4000.0,
        Case::NarrowPg | Case::NarrowSqlite => 100.0,
        Case::WidePg | Case::WideSqlite => 600.0,
    }
}

/// Fresh processes per case: time to first row is noisier, so it gets more.
pub fn runs(case: Case) -> usize {
    match case {
        Case::TtfrPg | Case::TtfrSqlite => 5,
        _ => 3,
    }
}

/// Refuses a sample taken from a table smaller than the spec's shape.
pub fn check_rows(case: Case, rows: u64) -> Result<()> {
    let (want, exact) = match case {
        Case::TtfrPg | Case::TtfrSqlite => (1, false),
        Case::StreamPg | Case::StreamSqlite => (10_000_000, true),
        _ => (1_000_000, true),
    };
    if (exact && rows == want) || (!exact && rows >= want) {
        return Ok(());
    }
    bail!(
        "{} read {rows} rows, expected {}{want}; load the stress tables with `cargo xtask db up --datasets stress`",
        case.name(),
        if exact { "" } else { "at least " }
    )
}

/// All samples of one case.
pub struct Measure {
    pub case: Case,
    pub samples: Vec<f64>,
}

impl Measure {
    fn met(&self) -> bool {
        median(&self.samples) < target(self.case)
    }
}

/// One report line for a measure; the values in run order show which run was cold.
pub fn line(m: &Measure) -> String {
    let min = m.samples.iter().copied().fold(f64::INFINITY, f64::min);
    let max = m.samples.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let each: Vec<String> = m.samples.iter().map(|v| format!("{v:.2}")).collect();
    format!(
        "{name:<14} median {median:>10.2} {unit:<6} min {min:.2} max {max:.2} runs {n} [{each}] target < {target} {status}",
        each = each.join(" "),
        name = m.case.name(),
        median = median(&m.samples),
        unit = unit(m.case),
        n = m.samples.len(),
        target = target(m.case),
        status = if m.met() { "ok" } else { "MISSED" },
    )
}

/// The full report text and whether every target was met.
pub fn report(measures: &[Measure]) -> (String, bool) {
    let mut text = String::from("dbento bench (release; median of fresh processes)\n");
    for m in measures {
        text += &line(m);
        text.push('\n');
    }
    let met = measures.iter().filter(|m| m.met()).count();
    text += &format!("{met} of {} targets met\n", measures.len());
    (text, met == measures.len())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_targets_are_the_spec_table() {
        let got: Vec<_> = Case::ALL
            .iter()
            .map(|&c| (c.name(), target(c), unit(c), runs(c)))
            .collect();
        assert_eq!(
            got,
            [
                ("ttfr_pg", 5.0, "ms", 5),
                ("ttfr_sqlite", 1.0, "ms", 5),
                ("stream_pg", 4000.0, "ms", 3),
                ("stream_sqlite", 4000.0, "ms", 3),
                ("narrow_pg", 100.0, "MiB/1M", 3),
                ("narrow_sqlite", 100.0, "MiB/1M", 3),
                ("wide_pg", 600.0, "MiB/1M", 3),
                ("wide_sqlite", 600.0, "MiB/1M", 3),
            ]
        );
    }

    #[test]
    fn short_stress_tables_are_refused() {
        assert!(check_rows(Case::TtfrPg, 1).is_ok());
        assert!(check_rows(Case::TtfrSqlite, 0).is_err());
        assert!(check_rows(Case::StreamPg, 10_000_000).is_ok());
        assert!(check_rows(Case::StreamSqlite, 9_999_999).is_err());
        assert!(check_rows(Case::NarrowPg, 1_000_000).is_ok());
        let err = check_rows(Case::WideSqlite, 200).unwrap_err();
        assert!(format!("{err:#}").contains("--datasets stress"), "{err:#}");
    }

    #[test]
    fn a_line_is_stable_and_a_target_is_strict() {
        let ok = Measure {
            case: Case::TtfrPg,
            samples: vec![0.7, 0.5, 0.6, 0.9, 0.55],
        };
        assert_eq!(
            line(&ok),
            "ttfr_pg        median       0.60 ms     min 0.50 max 0.90 runs 5 [0.70 0.50 0.60 0.90 0.55] target < 5 ok"
        );
        let missed = Measure {
            case: Case::NarrowPg,
            samples: vec![101.0, 99.0, 120.0],
        };
        assert_eq!(
            line(&missed),
            "narrow_pg      median     101.00 MiB/1M min 99.00 max 120.00 runs 3 [101.00 99.00 120.00] target < 100 MISSED"
        );
        let edge = Measure {
            case: Case::WideSqlite,
            samples: vec![600.0, 600.0, 600.0],
        };
        assert!(line(&edge).ends_with("MISSED"));
    }

    #[test]
    fn the_report_counts_the_targets_met() {
        let ms = [
            Measure {
                case: Case::TtfrPg,
                samples: vec![0.7, 0.5, 0.6, 0.9, 0.55],
            },
            Measure {
                case: Case::NarrowPg,
                samples: vec![101.0, 99.0, 120.0],
            },
        ];
        let (text, all) = report(&ms);
        assert!(!all);
        assert_eq!(
            text,
            format!(
                "dbento bench (release; median of fresh processes)\n{}\n{}\n1 of 2 targets met\n",
                line(&ms[0]),
                line(&ms[1])
            )
        );
        assert!(report(&ms[..1]).1);
    }
}
