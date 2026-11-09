use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use dbento_bench::cases::{Case, PgUrl, Targets, measure};
use dbento_bench::measure::{MemReading, Sample, run_child, sample_args};
use dbento_bench::report::{Measure, check_rows, report, runs};
use dbento_core::Engine;

const PG_URL_ENV: &str = "DBENTO_PG_17_STRESS_URL";
const SQLITE_ENV: &str = "DBENTO_STRESS_SQLITE";

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Allocates and touches this many MiB, then prints the VmHWM growth.
    Probe { mib: usize },
    /// Runs one case in this process and prints its sample line.
    Sample {
        case: String,
        #[arg(long)]
        pg_url: Option<String>,
        #[arg(long)]
        sqlite: Option<std::path::PathBuf>,
    },
    /// Measures every case in fresh child processes and prints the report.
    /// Exits 0 when every target is met, 1 when one is missed, 2 on an error.
    Run {
        #[arg(long)]
        pg_url: Option<String>,
        #[arg(long)]
        sqlite: Option<std::path::PathBuf>,
        /// Comma-separated case names to run; default is all.
        #[arg(long, value_delimiter = ',')]
        only: Vec<String>,
    },
}

fn main() {
    // An error is exit 2: returning Err from main would exit 1, which means "missed".
    match run_cmd(Cli::parse().cmd) {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("error: {e:#}");
            std::process::exit(2);
        }
    }
}

/// Runs one subcommand and returns the process exit code.
fn run_cmd(cmd: Cmd) -> Result<i32> {
    match cmd {
        Cmd::Probe { mib } => {
            let before = MemReading::now()?;
            // Filled with 1s: a zeroed Vec is calloc'd and not resident until touched.
            let buf = std::hint::black_box(vec![1u8; mib << 20]);
            let after = MemReading::now()?;
            std::hint::black_box(&buf);
            let s = Sample {
                case: "probe".into(),
                value: after.grown_kib_since(&before) as f64 / 1024.0,
                rows: 0,
            };
            println!("{}", s.to_line());
        }
        Cmd::Sample {
            case,
            pg_url,
            sqlite,
        } => {
            let case = Case::parse(&case).with_context(|| format!("unknown case {case:?}"))?;
            // The environment is the fallback, as clap's `env` feature is not enabled here.
            let pg_url = pg_url.or_else(|| std::env::var(PG_URL_ENV).ok());
            let sqlite = sqlite.or_else(|| std::env::var_os(SQLITE_ENV).map(Into::into));
            // Only a Postgres case needs the URL, so a bad one must not break SQLite cases.
            let pg = if case.engine() == Engine::Postgres {
                pg_url.as_deref().map(PgUrl::parse).transpose()?
            } else {
                None
            };
            let targets = Targets::new(pg, sqlite)?;
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()?;
            println!("{}", rt.block_on(measure(case, &targets))?.to_line());
        }
        Cmd::Run {
            pg_url,
            sqlite,
            only,
        } => {
            let cases = if only.is_empty() {
                Case::ALL.to_vec()
            } else {
                only.iter()
                    .map(|n| Case::parse(n).with_context(|| format!("unknown case {n:?}")))
                    .collect::<Result<Vec<Case>>>()?
            };
            let cases = cases.into_iter().fold(Vec::new(), |mut v, c| {
                if !v.contains(&c) {
                    v.push(c);
                }
                v
            });
            // Resolved here so `run` honours the environment, then handed to children by env.
            let pg_url = pg_url.or_else(|| std::env::var(PG_URL_ENV).ok());
            let sqlite = sqlite.or_else(|| std::env::var_os(SQLITE_ENV).map(Into::into));
            let exe = std::env::current_exe()?;
            let mut measures = Vec::new();
            for case in cases {
                let mut samples = Vec::new();
                for i in 1..=runs(case) {
                    eprintln!("{} run {i}/{}", case.name(), runs(case));
                    let args = sample_args(case.name(), sqlite.as_deref());
                    let envs: Vec<(&str, &str)> =
                        pg_url.iter().map(|u| (PG_URL_ENV, u.as_str())).collect();
                    // Belt and braces: scrub the URL if a child ever echoes it in its failure text.
                    let s = run_child(&exe, &args, &envs).map_err(|e| {
                        let text = format!("{e:#}");
                        anyhow::anyhow!(match &pg_url {
                            Some(u) => text.replace(u.as_str(), "<pg-url>"),
                            None => text,
                        })
                    })?;
                    check_rows(case, s.rows)?;
                    samples.push(s.value);
                }
                measures.push(Measure { case, samples });
            }
            let (text, all_met) = report(&measures);
            print!("{text}");
            return Ok(if all_met { 0 } else { 1 });
        }
    }
    Ok(0)
}
