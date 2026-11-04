//! `cargo xtask app-bench`: builds the app with its `bench` feature, runs each case in a
//! private virtual KWin, and judges first paint, memory and cancel against their targets.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

/// One bench case: the app's mode, the workspace connection and the statement.
#[derive(Debug, Clone, Copy)]
pub struct Case {
    pub name: &'static str,
    pub mode: &'static str,
    pub connection: &'static str,
    pub sql: &'static str,
}

const BIG: &str = "SELECT * FROM big WHERE id <= 1000000";
const WIDE: &str = "SELECT * FROM wide";

/// Every case, in the order they run.
pub const CASES: [Case; 5] = [
    Case {
        name: "load_big_pg",
        mode: "load",
        connection: "pg",
        sql: BIG,
    },
    Case {
        name: "load_big_sqlite",
        mode: "load",
        connection: "lite",
        sql: BIG,
    },
    Case {
        name: "load_wide_pg",
        mode: "load",
        connection: "pg",
        sql: WIDE,
    },
    Case {
        name: "load_wide_sqlite",
        mode: "load",
        connection: "lite",
        sql: WIDE,
    },
    Case {
        name: "cancel_pg",
        mode: "cancel",
        connection: "pg",
        sql: "SELECT pg_sleep(60)",
    },
];

/// The app's `BenchReport` as read back; the field names are the wire contract.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct AppReport {
    pub case: String,
    pub rows: u64,
    pub heap_bytes: u64,
    pub hwm_before_kib: u64,
    pub hwm_loaded_kib: u64,
    pub hwm_end_kib: u64,
    pub first_paint_ms: Option<f64>,
    pub cancel_ms: Option<f64>,
    pub end: String,
}

/// One measure of one case against its limit.
#[derive(Debug, Clone, PartialEq)]
pub struct Verdict {
    pub case: String,
    pub measure: &'static str,
    pub value: f64,
    pub limit: f64,
    pub ok: bool,
}

const FIRST_PAINT_MS: f64 = 100.0;
const CANCEL_MS: f64 = 1000.0;
const GROWTH_FACTOR: f64 = 1.10;
// A harness safeguard against a hung case, not a target.
const CASE_TIMEOUT: Duration = Duration::from_secs(60 * 60);

/// KWin's arguments for a private 1280 x 800 virtual output that runs `app` as its session.
pub fn kwin_args(app: &Path, socket: &str) -> Vec<String> {
    [
        "--virtual",
        "--no-lockscreen",
        "--no-global-shortcuts",
        "--socket",
        socket,
        "--width",
        "1280",
        "--height",
        "800",
        "--exit-with-session",
    ]
    .into_iter()
    .map(String::from)
    .chain([app.display().to_string()])
    .collect()
}

/// The median on `case`'s line of a dbento-bench report, if it has one.
pub fn parse_core_median(stdout: &str, case: &str) -> Option<f64> {
    stdout.lines().find_map(|line| {
        let mut words = line.split_whitespace();
        if words.next() != Some(case) || words.next() != Some("median") {
            return None;
        }
        words.next()?.parse().ok()
    })
}

/// Judges each report: every load must complete, big loads paint first within 100 ms,
/// wide loads grow at most 1.10 x the core median, and cancel lands within 1 s. Err when a
/// wide case lacks its core figure.
pub fn evaluate(reports: &[AppReport], core: &[(&str, f64)]) -> Result<Vec<Verdict>> {
    let mut verdicts = Vec::new();
    for r in reports {
        let mut judge = |measure, value: f64, limit, ok: fn(f64, f64) -> bool| {
            verdicts.push(Verdict {
                case: r.case.clone(),
                measure,
                value,
                limit,
                // NaN stands for a missing figure and is never within a limit.
                ok: !value.is_nan() && ok(value, limit),
            });
        };
        let at_most = |v: f64, l: f64| v <= l;
        let under = |v: f64, l: f64| v < l;
        if r.case.starts_with("load_") {
            let complete = if r.end == "Complete" { 1.0 } else { 0.0 };
            judge("complete", complete, 1.0, |v, l| v >= l);
        }
        if r.case.starts_with("load_big_") {
            judge(
                "first_paint_ms",
                r.first_paint_ms.unwrap_or(f64::NAN),
                FIRST_PAINT_MS,
                under,
            );
        }
        if let Some(core_case) = r
            .case
            .strip_prefix("load_")
            .filter(|c| c.starts_with("wide_"))
        {
            let Some(&(_, median)) = core.iter().find(|(c, _)| *c == core_case) else {
                bail!("{}: no core figure for {core_case}", r.case);
            };
            judge("growth_mib", growth_mib(r), GROWTH_FACTOR * median, at_most);
        }
        if r.case == "cancel_pg" {
            judge(
                "cancel_ms",
                r.cancel_ms.unwrap_or(f64::NAN),
                CANCEL_MS,
                under,
            );
        }
    }
    Ok(verdicts)
}

// VmHWM growth over the run in MiB, scaled to a million rows like the core figure.
fn growth_mib(r: &AppReport) -> f64 {
    if r.rows == 0 {
        return f64::NAN;
    }
    let mib = r.hwm_end_kib.saturating_sub(r.hwm_before_kib) as f64 / 1024.0;
    mib * 1_000_000.0 / r.rows as f64
}

/// Runs the selected cases (all when empty) and returns 0 when every target is met, 1 when
/// one is missed, 2 is left to the caller for an Err.
pub fn run(cases: &[String]) -> Result<i32> {
    let selected = select(cases)?;
    if find_on_path("kwin_wayland").is_none() {
        bail!("kwin_wayland is not on PATH");
    }
    for case in &selected {
        let var = target_var(case);
        if var.is_some_and(|v| std::env::var_os(v).is_none()) {
            bail!("{} needs {} set", case.name, var.unwrap_or_default());
        }
    }
    let root = crate::workspace_root();
    let target =
        std::env::var_os("CARGO_TARGET_DIR").map_or_else(|| root.join("target"), PathBuf::from);
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    eprintln!("app-bench: building the release app with the bench feature");
    let status = Command::new(&cargo)
        .current_dir(&root)
        .args([
            "build",
            "--release",
            "--locked",
            "-p",
            "dbento-app",
            "--features",
            "bench",
        ])
        .status()
        .with_context(|| format!("spawning {cargo}"))?;
    if !status.success() {
        bail!("building dbento-app failed ({status})");
    }
    let out_dir = target.join("app-bench");
    std::fs::create_dir_all(&out_dir)?;
    let app = target.join("release").join("dbento-app");
    let mut reports = Vec::new();
    let core = core_medians(&cargo, &root, &selected)?;
    for case in &selected {
        eprintln!("app-bench: {}", case.name);
        reports.push(run_case(case, &app, &out_dir)?);
    }
    let core: Vec<(&str, f64)> = core.iter().map(|(c, m)| (*c, *m)).collect();
    let verdicts = evaluate(&reports, &core)?;
    for v in &verdicts {
        let status = if v.ok { "ok" } else { "MISSED" };
        println!(
            "{:<20} {:<15} {:>10.2} limit {:>9.2} {status}",
            v.case, v.measure, v.value, v.limit
        );
    }
    let met = verdicts.iter().filter(|v| v.ok).count();
    println!("{met} of {} targets met", verdicts.len());
    Ok(if met == verdicts.len() { 0 } else { 1 })
}

// The named cases, in the order named; an unknown name is an error listing them all.
fn select(names: &[String]) -> Result<Vec<Case>> {
    if names.is_empty() {
        return Ok(CASES.to_vec());
    }
    names
        .iter()
        .map(|n| {
            CASES
                .iter()
                .find(|c| c.name == n)
                .copied()
                .with_context(|| {
                    let all: Vec<_> = CASES.iter().map(|c| c.name).collect();
                    format!("unknown case {n:?}; the cases are {}", all.join(", "))
                })
        })
        .collect()
}

fn target_var(case: &Case) -> Option<&'static str> {
    match case.connection {
        "pg" => Some("DBENTO_PG_17_STRESS_URL"),
        "lite" => Some("DBENTO_STRESS_SQLITE"),
        _ => None,
    }
}

fn find_on_path(program: &str) -> Option<PathBuf> {
    std::env::split_paths(&std::env::var_os("PATH")?)
        .map(|dir| dir.join(program))
        .find(|p| p.is_file())
}

// The same-run core memory figure for each selected wide case, from dbento-bench's report.
fn core_medians(cargo: &str, root: &Path, selected: &[Case]) -> Result<Vec<(&'static str, f64)>> {
    let wide: Vec<&'static str> = selected
        .iter()
        .filter_map(|c| {
            c.name
                .strip_prefix("load_")
                .filter(|n| n.starts_with("wide_"))
        })
        .collect();
    if wide.is_empty() {
        return Ok(Vec::new());
    }
    eprintln!("app-bench: core figure for {}", wide.join(", "));
    let output = Command::new(cargo)
        .current_dir(root)
        .args(crate::bench::cargo_args(&["--only".into(), wide.join(",")]))
        .stderr(Stdio::inherit())
        .output()
        .with_context(|| format!("spawning {cargo}"))?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    eprint!("{stdout}");
    // Exit 1 is a missed core target, which still yields a figure to compare against.
    if !matches!(output.status.code(), Some(0 | 1)) {
        bail!("dbento-bench failed ({})", output.status);
    }
    wide.into_iter()
        .map(|c| {
            parse_core_median(&stdout, c)
                .map(|m| (c, m))
                .with_context(|| format!("dbento-bench printed no median for {c}"))
        })
        .collect()
}

// Hosts the app in its own KWin with the case's job file, then reads back its report.
// KWin gets a private config home so it never writes the desktop's output config.
fn run_case(case: &Case, app: &Path, out_dir: &Path) -> Result<AppReport> {
    let out = out_dir.join(format!("{}.json", case.name));
    let config = out_dir.join(format!("{}.config.json", case.name));
    let log = out_dir.join(format!("{}.log", case.name));
    let kwin_config = out_dir.join("kwin-config");
    std::fs::create_dir_all(&kwin_config)?;
    match std::fs::remove_file(&out) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
        _ => {}
    }
    let job = serde_json::json!({
        "case": case.name,
        "mode": case.mode,
        "connection": case.connection,
        "sql": case.sql,
        "out": out,
    });
    std::fs::write(&config, serde_json::to_vec_pretty(&job)?)?;
    let socket = format!("dbento-bench-{}-{}", std::process::id(), case.name);
    let log_file = std::fs::File::create(&log)?;
    let mut kwin = Command::new("kwin_wayland")
        .args(kwin_args(app, &socket))
        .env_remove("DISPLAY")
        .env_remove("WAYLAND_DISPLAY")
        .env("XDG_CONFIG_HOME", &kwin_config)
        .env("DBENTO_APP_BENCH", &config)
        .stdin(Stdio::null())
        .stdout(log_file.try_clone()?)
        .stderr(log_file)
        .spawn()
        .context("spawning kwin_wayland")?;
    let started = Instant::now();
    while kwin.try_wait()?.is_none() {
        if started.elapsed() > CASE_TIMEOUT {
            let _ = kwin.kill();
            let _ = kwin.wait();
            bail!(
                "{} still running after {} min; see {}",
                case.name,
                CASE_TIMEOUT.as_secs() / 60,
                log.display()
            );
        }
        std::thread::sleep(Duration::from_millis(200));
    }
    let bytes = std::fs::read(&out)
        .with_context(|| format!("{} wrote no report; see {}", case.name, log.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("reading {}", out.display()))
}
