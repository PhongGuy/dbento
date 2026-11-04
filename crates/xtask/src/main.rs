use clap::{Parser, Subcommand};
use xtask::db;

#[derive(Parser)]
#[command(about = "Repository automation for dbento")]
struct Cli {
    #[command(subcommand)]
    command: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Manage the Postgres, MySQL, MariaDB and SQL Server matrix containers.
    Db {
        #[command(subcommand)]
        action: Action,
    },
    /// Build the demo's SQLite database from its seed script, replacing `out`.
    DemoDb {
        seed: std::path::PathBuf,
        out: std::path::PathBuf,
    },
    /// Build and run the release bench against the stress tables; flags pass through.
    Bench {
        #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
        args: Vec<String>,
    },
    /// Build the app's bench mode and judge first paint, memory and cancel in a virtual KWin.
    AppBench {
        /// Comma-separated case names to run; default is all.
        #[arg(long, value_delimiter = ',')]
        cases: Vec<String>,
    },
    /// Render the PNG, ICO and ICNS exports into assets/icon/generated.
    Icons {
        /// Report exports that differ from a fresh render instead of writing them.
        #[arg(long)]
        check: bool,
    },
    /// Type-check the headless crates for Windows through mingw, skipping when it is absent.
    CheckWindows {
        /// Fail instead of skipping when the cross compiler or target is missing (CI).
        #[arg(long)]
        require: bool,
    },
    /// Render THIRD-PARTY-NOTICES.html through cargo-about, failing on an unaccepted licence.
    Notices {
        /// Generate into a temporary directory and keep nothing (CI gate).
        #[arg(long)]
        check: bool,
        /// Where the notices page is written.
        #[arg(long, required_unless_present = "check")]
        out: Option<std::path::PathBuf>,
    },
    /// Write the complete cargo-packager config, with signing from the environment.
    PackagerConfig {
        /// Directory cargo-packager writes packages into.
        #[arg(long)]
        out_dir: std::path::PathBuf,
        /// File the JSON config is written to, readable only by the owner.
        #[arg(long)]
        output: std::path::PathBuf,
        /// The generated THIRD-PARTY-NOTICES.html, added to the resources.
        #[arg(long)]
        notices: std::path::PathBuf,
        /// Target triple whose release directory holds the binaries.
        #[arg(long)]
        target: Option<String>,
    },
}

#[derive(Subcommand)]
enum Action {
    /// Start the servers (loading Chinook into Postgres) and print `export` lines on stdout.
    Up {
        /// postgres, mysql, mariadb or mssql (needs DBENTO_MSSQL_ACCEPT_EULA=Y).
        #[arg(long, default_value = "postgres")]
        engine: String,
        /// Comma list of versions; each engine has its own default.
        #[arg(long)]
        versions: Option<String>,
        #[arg(long, default_value = "podman")]
        runtime: String,
        /// Comma list from pagila, airlines, airlines-medium, airlines-big, stress. Needs
        /// `curl` and `unzip`; stress takes about 1.2 GB of tmpfs per server.
        #[arg(long, default_value = "")]
        datasets: String,
    },
    /// Remove the matrix containers and orphaned test containers.
    Down {
        #[arg(long, default_value = "podman")]
        runtime: String,
    },
}

fn main() -> anyhow::Result<()> {
    match Cli::parse().command {
        Cmd::Db {
            action:
                Action::Up {
                    engine,
                    versions,
                    runtime,
                    datasets,
                },
        } => up(&engine, versions.as_deref(), &runtime, &datasets),
        Cmd::Db {
            action: Action::Down { runtime },
        } => db::down(&runtime),
        Cmd::DemoDb { seed, out } => xtask::demo::build_db(&seed, &out),
        Cmd::Bench { args } => exit(xtask::bench::run(&args)),
        Cmd::AppBench { cases } => exit(xtask::app_bench::run(&cases)),
        Cmd::Icons { check } => exit(xtask::icons::run(check)),
        Cmd::CheckWindows { require } => exit(xtask::windows_check::run(require)),
        Cmd::Notices { check, out } => exit(xtask::notices::run(check, out.as_deref())),
        Cmd::PackagerConfig {
            out_dir,
            output,
            notices,
            target,
        } => xtask::packager_config::run(&out_dir, &output, target.as_deref(), &notices),
    }
}

/// `db up` for one engine: Postgres loads datasets, the MySQL family takes none, and SQL Server
/// is the one 2022 server, started only once its licence variable is set.
fn up(engine: &str, versions: Option<&str>, runtime: &str, datasets: &str) -> anyhow::Result<()> {
    if engine == "mssql" {
        anyhow::ensure!(
            datasets.is_empty() && versions.is_none(),
            "--engine mssql takes neither --datasets nor --versions"
        );
        return db::up_mssql(runtime, |k| std::env::var(k).ok());
    }
    let Some(my) = db::parse_engine(engine)? else {
        return db::up(
            runtime,
            &db::parse_versions(versions.unwrap_or(db::DEFAULT_VERSIONS))?,
            &xtask::datasets::parse_datasets(datasets)?,
        );
    };
    anyhow::ensure!(
        datasets.is_empty(),
        "--datasets loads only into Postgres, not --engine {engine}"
    );
    let default = match my {
        db::MyEngine::Mysql => db::DEFAULT_MYSQL_VERSIONS,
        db::MyEngine::Mariadb => db::DEFAULT_MARIADB_VERSIONS,
    };
    db::up_mysql(
        runtime,
        my,
        &db::parse_my_versions(versions.unwrap_or(default))?,
    )
}

// An error is exit 2: returning Err from main would exit 1, which means "missed".
fn exit(outcome: anyhow::Result<i32>) -> ! {
    match outcome {
        Ok(code) => std::process::exit(code),
        Err(e) => {
            eprintln!("error: {e:#}");
            std::process::exit(2);
        }
    }
}
