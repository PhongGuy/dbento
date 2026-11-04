//! The memory method and the one-line protocol between the bench and its children.
//!
//! VmHWM only ever grows, so each memory case runs in a fresh child process and
//! reports its growth as a `sample` line on stdout.

use anyhow::{Context, Result, anyhow, bail, ensure};
use std::path::Path;
use std::process::Command;

/// Reads the `VmHWM:` line of a `/proc/<pid>/status` text, in kB.
pub fn parse_vm_hwm_kib(status: &str) -> Option<u64> {
    let rest = status.lines().find_map(|l| l.strip_prefix("VmHWM:"))?;
    rest.trim().strip_suffix("kB")?.trim().parse().ok()
}

/// The peak resident set of this process so far, in KiB (Linux only).
pub fn vm_hwm_kib() -> Result<u64> {
    let status =
        std::fs::read_to_string("/proc/self/status").context("reading /proc/self/status")?;
    parse_vm_hwm_kib(&status).ok_or_else(|| anyhow!("no VmHWM line in /proc/self/status"))
}

/// The median of `samples`; an even count averages the middle pair. Panics on empty input.
pub fn median(samples: &[f64]) -> f64 {
    assert!(!samples.is_empty(), "median of no samples");
    let mut v = samples.to_vec();
    v.sort_by(f64::total_cmp);
    let mid = v.len() / 2;
    if v.len() % 2 == 1 {
        v[mid]
    } else {
        (v[mid - 1] + v[mid]) / 2.0
    }
}

/// One measurement from one child process.
#[derive(Debug, Clone, PartialEq)]
pub struct Sample {
    pub case: String,
    pub value: f64,
    pub rows: u64,
}

impl Sample {
    /// The wire form: `sample case=<c> value=<v> rows=<n>`.
    pub fn to_line(&self) -> String {
        format!(
            "sample case={} value={} rows={}",
            self.case, self.value, self.rows
        )
    }

    /// Parses the wire form; every field is required.
    pub fn parse_line(line: &str) -> Result<Sample> {
        let mut words = line.split_whitespace();
        if words.next() != Some("sample") {
            bail!("not a sample line: {line:?}");
        }
        let (mut case, mut value, mut rows) = (None, None, None);
        for w in words {
            match w.split_once('=') {
                Some(("case", v)) => case = Some(v.to_string()),
                Some(("value", v)) => {
                    let f = v
                        .parse::<f64>()
                        .with_context(|| format!("value in {line:?}"))?;
                    ensure!(f.is_finite(), "value in {line:?} is not finite");
                    value = Some(f)
                }
                Some(("rows", v)) => {
                    rows = Some(
                        v.parse::<u64>()
                            .with_context(|| format!("rows in {line:?}"))?,
                    )
                }
                _ => bail!("unexpected field {w:?} in {line:?}"),
            }
        }
        match (case, value, rows) {
            (Some(case), Some(value), Some(rows)) => Ok(Sample { case, value, rows }),
            _ => bail!("sample line misses a field: {line:?}"),
        }
    }
}

/// The arguments of one `sample` child. The Postgres URL is not among them: it travels in the
/// child's environment so the password never shows in `ps`.
pub fn sample_args(case: &str, sqlite: Option<&Path>) -> Vec<String> {
    let mut args = vec!["sample".to_owned(), case.to_owned()];
    if let Some(p) = sqlite {
        args.extend(["--sqlite".to_owned(), p.display().to_string()]);
    }
    args
}

/// Runs `exe args` with `envs` added to its environment, and parses the last non-empty stdout line as a [`Sample`].
/// A non-zero exit is an error carrying the child's stderr.
pub fn run_child(exe: &Path, args: &[String], envs: &[(&str, &str)]) -> Result<Sample> {
    let out = Command::new(exe)
        .args(args)
        .envs(envs.iter().copied())
        .output()
        .with_context(|| format!("spawning {}", exe.display()))?;
    if !out.status.success() {
        bail!(
            "{} {} failed ({}): {}",
            exe.display(),
            args.join(" "),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    let stdout = String::from_utf8_lossy(&out.stdout);
    let line = stdout
        .lines()
        .rev()
        .find(|l| !l.trim().is_empty())
        .ok_or_else(|| anyhow!("child printed nothing"))?;
    Sample::parse_line(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vm_hwm_is_read_from_its_line() {
        let status = "Name:\tx\nVmPeak:\t  9000 kB\nVmHWM:\t    4812 kB\nVmRSS:\t 4700 kB\n";
        assert_eq!(parse_vm_hwm_kib(status), Some(4812));
        assert_eq!(parse_vm_hwm_kib("VmRSS:\t1 kB\n"), None);
        assert_eq!(parse_vm_hwm_kib("VmHWM:\tlots kB\n"), None);
    }

    #[test]
    fn child_args_carry_no_postgres_url() {
        let args = sample_args("ttfr_pg", Some(Path::new("/x/s.sqlite")));
        assert_eq!(args, ["sample", "ttfr_pg", "--sqlite", "/x/s.sqlite"]);
        assert_eq!(sample_args("ttfr_pg", None), ["sample", "ttfr_pg"]);
    }

    #[test]
    fn the_median_sorts_and_averages_the_middle_pair() {
        assert_eq!(median(&[3.0, 1.0, 2.0]), 2.0);
        assert_eq!(median(&[4.0, 1.0, 3.0, 2.0]), 2.5);
        assert_eq!(median(&[7.5]), 7.5);
    }

    #[test]
    fn a_sample_line_round_trips() {
        let s = Sample {
            case: "ttfr_pg".into(),
            value: 0.123456789,
            rows: 1024,
        };
        assert_eq!(
            s.to_line(),
            "sample case=ttfr_pg value=0.123456789 rows=1024"
        );
        assert_eq!(Sample::parse_line(&s.to_line()).unwrap(), s);
    }

    #[test]
    fn a_malformed_sample_line_is_an_error() {
        for bad in [
            "",
            "case=x value=1 rows=1",
            "sample case=x value=abc rows=1",
            "sample case=x value=1",
            "sample value=1 rows=1",
            "sample case=x value=1 rows=-1",
            "sample case=x value=NaN rows=1",
            "sample case=x value=inf rows=1",
        ] {
            assert!(Sample::parse_line(bad).is_err(), "{bad:?}");
        }
    }
}
