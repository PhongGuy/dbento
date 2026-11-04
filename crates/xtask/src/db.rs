//! `cargo xtask db`: long-lived matrix containers: Postgres with Chinook loaded, MySQL and
//! MariaDB servers, or one SQL Server 2022 behind the licence variable.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail, ensure};
use sha2::{Digest, Sha256};

use crate::datasets::{self, Choice};
use crate::stress;

pub const DEFAULT_VERSIONS: &str = "14,15,16,17,18";
pub const CHINOOK_PG_SHA256: &str =
    "e3fde5c1a5b51a2a91429a702c9ca6e69ba56e6c7f5e112724d70c3d03db695e";
pub const CHINOOK_SQLITE_SHA256: &str =
    "fdcb271b3e9c840216b09168752bddca973ed3917b40e49b603b15831114aea1";

/// Parses a comma list of major versions into a sorted, deduplicated list, each 14 to 99.
pub fn parse_versions(s: &str) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    for part in s.split(',') {
        let v: u8 = part
            .trim()
            .parse()
            .with_context(|| format!("bad version {part:?}"))?;
        ensure!((14..=99).contains(&v), "version {v} is outside 14..=99");
        out.push(v);
    }
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

/// The arguments after the runtime that start one labelled, tmpfs-backed server.
pub fn run_args(version: u8) -> Vec<String> {
    // Postgres 18 keeps its data in a versioned subdirectory, so the tmpfs covers the parent.
    let data = if version >= 18 {
        "/var/lib/postgresql"
    } else {
        "/var/lib/postgresql/data"
    };
    let mut args: Vec<String> = [
        "run",
        "-d",
        "--name",
        &format!("dbento-pg-{version}"),
        "--label",
        "dbento.role=matrix",
        "--label",
        &format!("dbento.version={version}"),
        "--tmpfs",
        data,
        "-e",
        "POSTGRES_PASSWORD=dbento",
        "-p",
        "127.0.0.1::5432",
        &format!("docker.io/library/postgres:{version}-alpine"),
    ]
    .map(String::from)
    .to_vec();
    for setting in [
        "fsync=off",
        "full_page_writes=off",
        "synchronous_commit=off",
    ] {
        args.extend(["-c".to_owned(), setting.to_owned()]);
    }
    args
}

/// The host port from the first line of `<runtime> port <name> 5432` output.
pub fn parse_port(port_output: &str) -> Result<u16> {
    let first = port_output.lines().next().unwrap_or("").trim();
    let (_, port) = first
        .rsplit_once(':')
        .with_context(|| format!("no host:port in {port_output:?}"))?;
    port.parse()
        .with_context(|| format!("bad port in {first:?}"))
}

fn pg_url(port: u16, db: &str) -> String {
    format!("postgres://postgres:dbento@127.0.0.1:{port}/{db}")
}

/// The two `export` lines for one server, from the output of `<runtime> port <name> 5432`.
pub fn export_lines(version: u8, port_output: &str) -> Result<[String; 2]> {
    let port = parse_port(port_output)?;
    Ok([
        format!(
            "export DBENTO_PG_{version}_URL={}",
            pg_url(port, "postgres")
        ),
        format!(
            "export DBENTO_PG_{version}_CHINOOK_URL={}",
            pg_url(port, "chinook")
        ),
    ])
}

/// The `export` line for one dataset database on one server.
pub fn dataset_export(version: u8, port: u16, label: &str, database: &str) -> String {
    format!(
        "export DBENTO_PG_{version}_{label}_URL={}",
        pg_url(port, database)
    )
}

/// Ids of test containers whose recorded `dbento.pid` is a process that no longer lives.
/// Containers without a numeric pid label are never listed.
pub fn orphans(listing: &str, alive: impl Fn(u32) -> bool) -> Vec<String> {
    listing
        .lines()
        .filter_map(|line| {
            let (id, labels) = line.trim().split_once(' ')?;
            let pid = labels
                .split(',')
                .find_map(|l| l.strip_prefix("dbento.pid="))?
                .parse()
                .ok()?;
            (!alive(pid)).then(|| id.to_owned())
        })
        .collect()
}

/// The runtime arguments that force-remove `names` together with their anonymous volumes.
pub fn remove_args<'a>(names: &[&'a str]) -> Vec<&'a str> {
    let mut args = vec!["rm", "-f", "-v"];
    args.extend(names);
    args
}

/// `remove_args` without the stop wait, which SQL Server always runs into by ignoring SIGTERM.
pub fn kill_args<'a>(names: &[&'a str]) -> Vec<&'a str> {
    let mut args = vec!["rm", "-f", "-t", "0", "-v"];
    args.extend(names);
    args
}

/// Lowercase hex of a digest.
pub fn hex(digest: impl AsRef<[u8]>) -> String {
    digest.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(Sha256::digest(bytes))
}

/// Builds the Chinook SQLite database at `path`, replacing any file already there.
pub fn build_sqlite_chinook(path: &Path) -> Result<()> {
    let sql = std::fs::read_to_string(chinook_dir().join("Chinook_Sqlite.sql"))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
        _ => {}
    }
    rusqlite::Connection::open(path)?
        .execute_batch(&sql)
        .context("loading Chinook into SQLite")
}

fn chinook_dir() -> std::path::PathBuf {
    crate::workspace_root().join("datasets/chinook")
}

fn verify_vendored() -> Result<Vec<u8>> {
    let pg = std::fs::read(chinook_dir().join("Chinook_PostgreSql.sql"))?;
    ensure!(
        sha256_hex(&pg) == CHINOOK_PG_SHA256,
        "Chinook_PostgreSql.sql does not match its pinned hash"
    );
    let lite = std::fs::read(chinook_dir().join("Chinook_Sqlite.sql"))?;
    ensure!(
        sha256_hex(&lite) == CHINOOK_SQLITE_SHA256,
        "Chinook_Sqlite.sql does not match its pinned hash"
    );
    Ok(pg)
}

/// Runs the runtime with `args`, returning stdout and failing with stderr on a non-zero exit.
fn capture(runtime: &str, args: &[&str]) -> Result<String> {
    let out = Command::new(runtime)
        .args(args)
        .output()
        .with_context(|| format!("running {runtime}"))?;
    ensure!(
        out.status.success(),
        "{runtime} {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

pub(crate) fn cache_dir() -> Result<std::path::PathBuf> {
    if let Some(x) = std::env::var_os("XDG_CACHE_HOME").filter(|v| !v.is_empty()) {
        return Ok(x.into());
    }
    let home = std::env::var_os("HOME").context("neither XDG_CACHE_HOME nor HOME is set")?;
    Ok(std::path::PathBuf::from(home).join(".cache"))
}

/// Starts one server per version, loads Chinook and the chosen datasets, and prints the
/// `export` lines on stdout. On failure, removes every container this run started.
///
/// Prerequisites for `--datasets`: `curl` (downloads) and `unzip` (airlines) on the host.
/// Podman's default tmpfs is about half of RAM and `stress` takes about 1.2 GB per server, so
/// `--datasets stress` on the default five versions needs about 6 GB; use `--versions 17`.
pub fn up(runtime: &str, versions: &[u8], datasets: &[Choice]) -> Result<()> {
    let chinook_pg = verify_vendored()?;
    let root = cache_dir()?.join("dbento-datasets");

    // Downloads come first so a network failure never leaves containers behind.
    let mut prepared: Vec<(&'static datasets::Dataset, Vec<std::path::PathBuf>)> = Vec::new();
    let mut stress_chosen = false;
    for choice in datasets {
        match choice {
            Choice::Download(d) => {
                let dir = root.join(d.name);
                let mut files = Vec::new();
                for f in d.downloads {
                    eprintln!("fetching {}", f.file);
                    files.push(datasets::fetch(f, &dir)?);
                }
                std::fs::write(dir.join("SOURCE.txt"), datasets::source_note(d))?;
                prepared.push((d, files));
            }
            Choice::Stress => stress_chosen = true,
        }
    }
    let stress_sqlite = root.join("stress.sqlite");
    if stress_chosen
        && !stress::sqlite_stress_ready(&stress_sqlite, stress::BIG_ROWS, stress::WIDE_ROWS)
    {
        eprintln!("building the SQLite stress tables");
        stress::build_sqlite_stress(&stress_sqlite, stress::BIG_ROWS, stress::WIDE_ROWS)?;
    }

    // Built before any container starts, so its failure leaves nothing to clean up.
    let sqlite = root.join("chinook.sqlite");
    build_sqlite_chinook(&sqlite)?;

    let mut exports = Vec::new();
    let mut started = Vec::new();
    for &v in versions {
        let name = format!("dbento-pg-{v}");
        // Listed before the attempt, so a half-started container is removed too.
        started.push(name.clone());
        match start_one(runtime, v, &name, &chinook_pg, &prepared, stress_chosen) {
            Ok(lines) => exports.extend(lines),
            Err(e) => return Err(remove_started(runtime, &started, e, remove_args)),
        }
    }

    for line in exports {
        println!("{line}");
    }
    println!("export DBENTO_CHINOOK_SQLITE={}", sqlite.display());
    if stress_chosen {
        println!("export DBENTO_STRESS_SQLITE={}", stress_sqlite.display());
    }
    Ok(())
}

/// Runs psql inside `name` against `db`, reading the script from `stdin`; fails on any error.
fn psql(runtime: &str, name: &str, db: &str, stdin: Stdio) -> Result<std::process::Child> {
    Command::new(runtime)
        .args([
            "exec",
            "-i",
            name,
            "psql",
            "-X",
            "-q",
            "-v",
            "ON_ERROR_STOP=1",
            "-U",
            "postgres",
            "-d",
            db,
            "-f",
            "-",
        ])
        .stdin(stdin)
        .stdout(Stdio::null())
        .spawn()
        .with_context(|| format!("running {runtime} exec psql"))
}

fn psql_file(runtime: &str, name: &str, db: &str, file: &Path) -> Result<()> {
    let f = std::fs::File::open(file).with_context(|| format!("open {}", file.display()))?;
    let status = psql(runtime, name, db, Stdio::from(f))?.wait()?;
    ensure!(
        status.success(),
        "loading {} into {name}/{db} failed",
        file.display()
    );
    Ok(())
}

fn create_database(runtime: &str, name: &str, db: &str) -> Result<()> {
    capture(
        runtime,
        &[
            "exec",
            name,
            "psql",
            "-X",
            "-q",
            "-v",
            "ON_ERROR_STOP=1",
            "-U",
            "postgres",
            "-d",
            "postgres",
            "-c",
            &format!("CREATE DATABASE {db}"),
        ],
    )
    .map(drop)
}

/// Loads one downloaded dataset into server `name`.
pub fn load_download(
    runtime: &str,
    name: &str,
    d: &datasets::Dataset,
    files: &[std::path::PathBuf],
) -> Result<()> {
    eprintln!("loading {} into {name}", d.name);
    if d.database == "pagila" {
        create_database(runtime, name, "pagila")?;
        psql_file(runtime, name, "pagila", &files[0])?;
        return psql_file(runtime, name, "pagila", &files[1]);
    }
    // The dump opens with `DROP DATABASE demo`, which fails under ON_ERROR_STOP unless it exists.
    create_database(runtime, name, d.database)?;
    let mut unzip = Command::new("unzip")
        .arg("-p")
        .arg(&files[0])
        .stdout(Stdio::piped())
        .spawn()
        .context("run unzip (a host prerequisite for the airlines datasets; install unzip)")?;
    let mut load = match psql(
        runtime,
        name,
        "postgres",
        Stdio::from(unzip.stdout.take().expect("piped")),
    ) {
        Ok(load) => load,
        Err(e) => {
            // Nothing will read unzip's output, so stop and reap it.
            let _ = unzip.kill();
            let _ = unzip.wait();
            return Err(e);
        }
    };
    // psql goes first: a failed load starves unzip into SIGPIPE, which would mask the cause.
    let loaded = load.wait()?;
    let unzipped = unzip.wait()?;
    ensure!(loaded.success(), "loading {} into {name} failed", d.name);
    ensure!(unzipped.success(), "unzip failed ({unzipped})");
    Ok(())
}

fn load_stress(runtime: &str, name: &str) -> Result<()> {
    eprintln!("loading the stress tables into {name}");
    create_database(runtime, name, "stress")?;
    let script =
        stress::pg_stress_statements(stress::BIG_ROWS, stress::WIDE_ROWS).join(";\n") + ";\n";
    let mut child = psql(runtime, name, "stress", Stdio::piped())?;
    // A psql that died early breaks the pipe; reap it and report its failure, not the write's.
    let written = child
        .stdin
        .take()
        .expect("piped")
        .write_all(script.as_bytes());
    ensure!(
        child.wait()?.success(),
        "loading the stress tables into {name} failed"
    );
    written.context("writing the stress script to psql")?;
    Ok(())
}

/// Starts server `name` at version `v`, waits for it, loads Chinook and the datasets, and
/// returns its export lines.
fn start_one(
    runtime: &str,
    v: u8,
    name: &str,
    chinook_pg: &[u8],
    prepared: &[(&'static datasets::Dataset, Vec<std::path::PathBuf>)],
    stress_chosen: bool,
) -> Result<Vec<String>> {
    eprintln!("starting {name}");
    // Ignore failure: the container usually does not exist yet.
    let _ = Command::new(runtime)
        .args(remove_args(&[name]))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let args = run_args(v);
    capture(
        runtime,
        &args.iter().map(String::as_str).collect::<Vec<_>>(),
    )?;

    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        let ready = Command::new(runtime)
            .args([
                "exec",
                name,
                "pg_isready",
                "-q",
                "-h",
                "127.0.0.1",
                "-U",
                "postgres",
            ])
            .status()?
            .success();
        if ready {
            break;
        }
        ensure!(
            Instant::now() < deadline,
            "{name} was not ready within 60 s"
        );
        std::thread::sleep(Duration::from_millis(500));
    }

    eprintln!("loading Chinook into {name}");
    let mut child = Command::new(runtime)
        .args([
            "exec",
            "-i",
            name,
            "psql",
            "-X",
            "-q",
            "-v",
            "ON_ERROR_STOP=1",
            "-U",
            "postgres",
            "-d",
            "postgres",
            "-f",
            "-",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()?;
    child.stdin.take().expect("piped").write_all(chinook_pg)?;
    if !child.wait()?.success() {
        bail!("loading Chinook into {name} failed");
    }
    let port_output = capture(runtime, &["port", name, "5432"])?;
    let port = parse_port(&port_output)?;
    let mut lines = export_lines(v, &port_output)?.to_vec();
    for (d, files) in prepared {
        load_download(runtime, name, d, files)?;
        lines.push(dataset_export(v, port, d.env_label, d.database));
    }
    if stress_chosen {
        load_stress(runtime, name)?;
        lines.push(dataset_export(v, port, "STRESS", "stress"));
    }
    Ok(lines)
}

/// A MySQL-family engine for the matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MyEngine {
    Mysql,
    Mariadb,
}

impl MyEngine {
    fn name(self) -> &'static str {
        match self {
            MyEngine::Mysql => "mysql",
            MyEngine::Mariadb => "mariadb",
        }
    }
}

pub const DEFAULT_MYSQL_VERSIONS: &str = "8.4";
pub const DEFAULT_MARIADB_VERSIONS: &str = "11.4";

/// None for "postgres"; an error names every engine, SQL Server included (`up` takes it first).
pub fn parse_engine(s: &str) -> Result<Option<MyEngine>> {
    match s {
        "postgres" => Ok(None),
        "mysql" => Ok(Some(MyEngine::Mysql)),
        "mariadb" => Ok(Some(MyEngine::Mariadb)),
        other => bail!("unknown engine {other:?}; expected postgres, mysql, mariadb or mssql"),
    }
}

/// Parses a comma list of "MAJOR.MINOR" items into a sorted, deduplicated list.
pub fn parse_my_versions(s: &str) -> Result<Vec<String>> {
    let mut out = Vec::new();
    for part in s.split(',') {
        let part = part.trim();
        let parsed = part.split_once('.').and_then(|(major, minor)| {
            Some((major.parse::<u16>().ok()?, minor.parse::<u16>().ok()?))
        });
        let Some(pair) = parsed else {
            bail!("bad version {part:?}; expected MAJOR.MINOR");
        };
        out.push(pair);
    }
    // Numeric order, so 10.11 sorts after 9.7.
    out.sort_unstable();
    out.dedup();
    Ok(out.into_iter().map(|(a, b)| format!("{a}.{b}")).collect())
}

/// The version without its dot, as used in container names and variables: "10.11" -> "1011".
pub fn my_label(version: &str) -> String {
    version.replace('.', "")
}

/// The arguments after the runtime that start one labelled, tmpfs-backed MySQL-family server.
pub fn my_run_args(engine: MyEngine, version: &str) -> Vec<String> {
    let name = engine.name();
    let password = match engine {
        MyEngine::Mysql => "MYSQL_ROOT_PASSWORD=dbento",
        MyEngine::Mariadb => "MARIADB_ROOT_PASSWORD=dbento",
    };
    [
        "run",
        "-d",
        "--name",
        &format!("dbento-{name}-{version}"),
        "--label",
        "dbento.role=matrix",
        "--label",
        &format!("dbento.version={name}-{version}"),
        "--tmpfs",
        "/var/lib/mysql",
        "-e",
        password,
        "-p",
        "127.0.0.1::3306",
        &format!("docker.io/library/{name}:{version}"),
    ]
    .map(String::from)
    .to_vec()
}

/// The matrix line, plus DBENTO_MYSQL_URL / DBENTO_MARIADB_URL when `first`.
pub fn my_export_lines(
    engine: MyEngine,
    version: &str,
    port_output: &str,
    first: bool,
) -> Result<Vec<String>> {
    let port = parse_port(port_output)?;
    let var = engine.name().to_uppercase();
    let url = format!("mysql://root:dbento@127.0.0.1:{port}/");
    let mut lines = vec![format!(
        "export DBENTO_{var}_{}_URL={url}",
        my_label(version)
    )];
    if first {
        lines.push(format!("export DBENTO_{var}_URL={url}"));
    }
    Ok(lines)
}

/// Starts one MySQL-family server per version and prints the `export` lines on stdout, the
/// lowest version also as the single-flavor variable. On failure, removes what it started.
pub fn up_mysql(runtime: &str, engine: MyEngine, versions: &[String]) -> Result<()> {
    let mut exports = Vec::new();
    let mut started = Vec::new();
    for (i, v) in versions.iter().enumerate() {
        let name = format!("dbento-{}-{v}", engine.name());
        // Listed before the attempt, so a half-started container is removed too.
        started.push(name.clone());
        match start_my(runtime, engine, v, &name, i == 0) {
            Ok(lines) => exports.extend(lines),
            Err(e) => return Err(remove_started(runtime, &started, e, remove_args)),
        }
    }
    for line in exports {
        println!("{line}");
    }
    Ok(())
}

/// Builds the runtime arguments that remove the named containers.
type RemoveArgs = for<'a> fn(&[&'a str]) -> Vec<&'a str>;

/// Force-removes `started` with `rm`, attaching the outcome to the error that caused it.
fn remove_started(
    runtime: &str,
    started: &[String],
    e: anyhow::Error,
    rm: RemoveArgs,
) -> anyhow::Error {
    let names: Vec<&str> = started.iter().map(String::as_str).collect();
    match capture(runtime, &rm(&names)) {
        Ok(_) => e.context(format!("removed {}", names.join(", "))),
        Err(rm) => e.context(format!(
            "could not remove the started containers ({rm}); run `cargo xtask db down`"
        )),
    }
}

/// Starts server `name`, waits until it answers over TCP, and returns its export lines.
fn start_my(
    runtime: &str,
    engine: MyEngine,
    version: &str,
    name: &str,
    first: bool,
) -> Result<Vec<String>> {
    eprintln!("starting {name}");
    // Ignore failure: the container usually does not exist yet.
    let _ = Command::new(runtime)
        .args(remove_args(&[name]))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let args = my_run_args(engine, version);
    capture(
        runtime,
        &args.iter().map(String::as_str).collect::<Vec<_>>(),
    )?;

    // The entrypoint first runs a socket-only temporary server, so only a TCP answer means
    // the real server is up.
    let deadline = Instant::now() + Duration::from_secs(90);
    loop {
        let ready = Command::new(runtime)
            .args([
                "exec",
                name,
                engine.name(),
                "-h127.0.0.1",
                "--protocol=TCP",
                "-uroot",
                "-pdbento",
                "-e",
                "SELECT 1",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?
            .success();
        if ready {
            break;
        }
        ensure!(
            Instant::now() < deadline,
            "{name} was not ready within 90 s"
        );
        std::thread::sleep(Duration::from_millis(500));
    }
    let port_output = capture(runtime, &["port", name, "3306"])?;
    my_export_lines(engine, version, &port_output, first)
}

pub const MSSQL_IMAGE: &str = "mcr.microsoft.com/mssql/server:2022-CU27-ubuntu-22.04";
pub const MSSQL_PASSWORD: &str = "DbentoPass2022";
const MSSQL_NAME: &str = "dbento-mssql-2022";
const MSSQL_EULA_VAR: &str = "DBENTO_MSSQL_ACCEPT_EULA";

/// Ok only when DBENTO_MSSQL_ACCEPT_EULA is exactly "Y"; the error names the variable and says
/// setting it accepts the Microsoft SQL Server Developer edition licence.
pub fn mssql_eula(env: impl Fn(&str) -> Option<String>) -> Result<()> {
    ensure!(
        env(MSSQL_EULA_VAR).as_deref() == Some("Y"),
        "SQL Server needs {MSSQL_EULA_VAR}=Y; setting it accepts the Microsoft SQL Server \
         Developer edition licence"
    );
    Ok(())
}

/// The arguments after the runtime that start the labelled, tmpfs-backed, memory-capped server.
pub fn ms_run_args() -> Vec<String> {
    // The 3 GB container cap leaves headroom over the 2 GB the server is told it may use.
    [
        "run",
        "-d",
        "--name",
        MSSQL_NAME,
        "--label",
        "dbento.role=matrix",
        "--label",
        "dbento.version=mssql-2022",
        "--tmpfs",
        "/var/opt/mssql/data:rw,size=4G,U",
        "--memory",
        "3g",
        "-e",
        "ACCEPT_EULA=Y",
        "-e",
        "MSSQL_PID=Developer",
        "-e",
        "MSSQL_MEMORY_LIMIT_MB=2048",
        "-e",
        &format!("MSSQL_SA_PASSWORD={MSSQL_PASSWORD}"),
        "-p",
        "127.0.0.1::1433",
        MSSQL_IMAGE,
    ]
    .map(String::from)
    .to_vec()
}

/// `export DBENTO_MSSQL_URL=mssql://sa:<password>@127.0.0.1:<port>` from `podman port` output.
pub fn ms_export_lines(port_output: &str) -> Result<Vec<String>> {
    let port = parse_port(port_output)?;
    Ok(vec![format!(
        "export DBENTO_MSSQL_URL=mssql://sa:{MSSQL_PASSWORD}@127.0.0.1:{port}"
    )])
}

/// Checks the licence variable before touching the runtime, then starts `dbento-mssql-2022`,
/// waits up to 120 s for sqlcmd to answer, and prints the export line on stdout.
pub fn up_mssql(runtime: &str, env: impl Fn(&str) -> Option<String>) -> Result<()> {
    mssql_eula(env)?;
    let started = [MSSQL_NAME.to_owned()];
    match start_mssql(runtime) {
        Ok(lines) => {
            for line in lines {
                println!("{line}");
            }
            Ok(())
        }
        Err(e) => Err(remove_started(runtime, &started, e, kill_args)),
    }
}

/// Starts the server, waits until sqlcmd inside it answers, and returns its export line.
fn start_mssql(runtime: &str) -> Result<Vec<String>> {
    eprintln!("starting {MSSQL_NAME}");
    // Ignore failure: the container usually does not exist yet.
    let _ = Command::new(runtime)
        .args(kill_args(&[MSSQL_NAME]))
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
    let args = ms_run_args();
    capture(
        runtime,
        &args.iter().map(String::as_str).collect::<Vec<_>>(),
    )?;

    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        let ready = Command::new(runtime)
            .args([
                "exec",
                MSSQL_NAME,
                "/opt/mssql-tools18/bin/sqlcmd",
                "-C",
                "-S",
                "localhost",
                "-U",
                "sa",
                "-P",
                MSSQL_PASSWORD,
                "-b",
                "-Q",
                "SELECT 1",
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()?
            .success();
        if ready {
            break;
        }
        ensure!(
            Instant::now() < deadline,
            "{MSSQL_NAME} was not ready within 120 s"
        );
        std::thread::sleep(Duration::from_secs(1));
    }
    ms_export_lines(&capture(runtime, &["port", MSSQL_NAME, "1433"])?)
}

/// Removes the matrix containers, then test containers whose owning process has exited.
pub fn down(runtime: &str) -> Result<()> {
    let mut ids: Vec<String> = capture(
        runtime,
        &["ps", "-a", "-q", "--filter", "label=dbento.role=matrix"],
    )?
    .split_whitespace()
    .map(str::to_owned)
    .collect();
    let listing = capture(
        runtime,
        &[
            "ps",
            "-a",
            "--filter",
            "label=dbento.role=test",
            "--format",
            "{{.ID}} {{.Labels}}",
        ],
    )?;
    ids.extend(orphans(&listing, process_alive));
    if ids.is_empty() {
        return Ok(());
    }
    // Every container is killed outright: SQL Server would hold each removal for 10 s.
    let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
    capture(runtime, &kill_args(&ids))?;
    eprintln!("removed {} container(s)", ids.len());
    Ok(())
}

/// Linux checks `/proc`; elsewhere every process counts as alive, so nothing is an orphan.
fn process_alive(pid: u32) -> bool {
    if cfg!(target_os = "linux") {
        Path::new(&format!("/proc/{pid}")).exists()
    } else {
        true
    }
}
