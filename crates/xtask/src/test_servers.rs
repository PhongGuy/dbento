//! `cargo xtask test-servers`: one shared server per engine for a whole test run.
//!
//! Each server is labelled with its owner's pid, so `down` removes exactly what `up` started.
//! This module builds the runtime arguments, readiness probes and `export` lines; the testkit
//! reads the exported URL variables and uses the shared server instead of a container per test.

use std::io::{ErrorKind, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, ensure};

use crate::db::{
    MSSQL_IMAGE, MSSQL_PASSWORD, MSSQL_READY_PROBE, capture, ms_export_lines, mssql_eula,
    parse_port, remove_args, remove_started,
};

/// A database engine the shared servers cover.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Engine {
    Postgres,
    Mysql,
    Mariadb,
    Mongo,
    Redis,
    Dynamo,
    Surreal,
    Mssql,
}

/// The engines always started, in start order; SQL Server is added only by [`pending`].
pub const ENGINES: [Engine; 7] = [
    Engine::Postgres,
    Engine::Mysql,
    Engine::Mariadb,
    Engine::Mongo,
    Engine::Redis,
    Engine::Dynamo,
    Engine::Surreal,
];

impl Engine {
    /// The engine's part of the container name.
    pub fn name(self) -> &'static str {
        match self {
            Engine::Postgres => "postgres",
            Engine::Mysql => "mysql",
            Engine::Mariadb => "mariadb",
            Engine::Mongo => "mongo",
            Engine::Redis => "redis",
            Engine::Dynamo => "dynamo",
            Engine::Surreal => "surreal",
            Engine::Mssql => "mssql",
        }
    }

    /// The variable the testkit reads this engine's server URL from.
    pub fn url_var(self) -> &'static str {
        match self {
            Engine::Postgres => "DBENTO_PG_URL",
            Engine::Mysql => "DBENTO_MYSQL_URL",
            Engine::Mariadb => "DBENTO_MARIADB_URL",
            Engine::Mongo => "DBENTO_MONGO_URL",
            Engine::Redis => "DBENTO_REDIS_URL",
            Engine::Dynamo => "DBENTO_DYNAMO_URL",
            Engine::Surreal => "DBENTO_SURREAL_URL",
            Engine::Mssql => "DBENTO_MSSQL_URL",
        }
    }

    /// The image tag; it must match the tag the testkit starts per test, which a test checks.
    pub fn tag(self) -> &'static str {
        match self {
            Engine::Postgres => "17-alpine",
            Engine::Mysql => "8.4",
            Engine::Mariadb => "11.4",
            Engine::Mongo => "8.0",
            Engine::Redis => "9.0.7-alpine",
            Engine::Dynamo => "5.2.5",
            Engine::Surreal => "v3.3.5",
            Engine::Mssql => MSSQL_IMAGE
                .rsplit_once(':')
                .map_or(MSSQL_IMAGE, |(_, tag)| tag),
        }
    }

    /// The port the server listens on inside its container.
    pub fn port(self) -> u16 {
        match self {
            Engine::Postgres => 5432,
            Engine::Mysql | Engine::Mariadb => 3306,
            Engine::Mongo => 27018,
            Engine::Redis => 6379,
            Engine::Dynamo => 5000,
            Engine::Surreal => 8000,
            Engine::Mssql => 1433,
        }
    }

    /// The fully qualified image, so podman never asks which registry a short name means.
    fn image(self) -> String {
        let repo = match self {
            Engine::Postgres => "docker.io/library/postgres",
            Engine::Mysql => "docker.io/library/mysql",
            Engine::Mariadb => "docker.io/library/mariadb",
            Engine::Mongo => "docker.io/library/mongo",
            Engine::Redis => "docker.io/valkey/valkey",
            Engine::Dynamo => "docker.io/motoserver/moto",
            Engine::Surreal => "docker.io/surrealdb/surrealdb",
            Engine::Mssql => return MSSQL_IMAGE.to_owned(),
        };
        format!("{repo}:{}", self.tag())
    }
}

/// `dbento-test-<name>-<owner>`.
pub fn container_name(engine: Engine, owner: u32) -> String {
    format!("dbento-test-{}-{owner}", engine.name())
}

/// The label filter that selects every server `owner` started.
pub fn owner_filter(owner: u32) -> String {
    format!("label=dbento.test-servers={owner}")
}

fn strings(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| (*s).to_owned()).collect()
}

/// The `run` arguments after the runtime: the labels the testkit's orphan sweep knows, a
/// loopback port, then the engine's options, image and command.
pub fn run_args(engine: Engine, owner: u32) -> Vec<String> {
    let mut args = strings(&[
        "run",
        "-d",
        "--name",
        &container_name(engine, owner),
        "--label",
        "dbento.role=test",
        "--label",
        &format!("dbento.pid={owner}"),
        "--label",
        &format!("dbento.test-servers={owner}"),
        "-p",
        &format!("127.0.0.1::{}", engine.port()),
    ]);
    // A test server is thrown away after the run, so durability is off and most engines keep
    // their data on tmpfs; SQL Server keeps its on disk, as noted at its arm.
    let (options, command): (&[&str], &[&str]) = match engine {
        Engine::Postgres => (
            &[
                "--tmpfs",
                "/var/lib/postgresql/data",
                "-e",
                "POSTGRES_PASSWORD=dbento",
            ],
            &[
                "-c",
                "fsync=off",
                "-c",
                "full_page_writes=off",
                "-c",
                "synchronous_commit=off",
                "-c",
                "max_connections=500",
            ],
        ),
        Engine::Mysql => (
            &[
                "--tmpfs",
                "/var/lib/mysql",
                "-e",
                "MYSQL_ROOT_PASSWORD=dbento",
            ],
            &[],
        ),
        Engine::Mariadb => (
            &[
                "--tmpfs",
                "/var/lib/mysql",
                "-e",
                "MARIADB_ROOT_PASSWORD=dbento",
            ],
            &[],
        ),
        Engine::Mongo => (
            &[
                "--tmpfs",
                "/data/db:size=2g,mode=1777",
                "-e",
                "MONGO_INITDB_ROOT_USERNAME=root",
                "-e",
                "MONGO_INITDB_ROOT_PASSWORD=dbento",
                "-e",
                "GLIBC_TUNABLES=glibc.pthread.rseq=1",
            ],
            // Mongo logs every connection and login by default; at test-suite rates that
            // exhausts journald's rate limit, which every rootless container shares.
            &[
                "--wiredTigerCacheSizeGB",
                "0.5",
                "--port",
                "27018",
                "--quiet",
                "--setParameter",
                "enableDetailedConnectionHealthMetricLogLines=false",
            ],
        ),
        Engine::Redis => (&[], &["valkey-server", "--save", "", "--appendonly", "no"]),
        Engine::Dynamo => (&[], &[]),
        // The in-memory engine keeps what the heavy seeds allocate, which outgrows a 2 GiB cap
        // within one suite run and kills the shared server.
        Engine::Surreal => (
            &[
                "-e",
                "SURREAL_USER=root",
                "-e",
                "SURREAL_PASS=dbento",
                "-e",
                "SURREAL_PATH=memory",
                "-e",
                "SURREAL_CAPS_ALLOW_ALL=true",
                "--memory",
                "8g",
            ],
            &["start"],
        ),
        // The password option is built at run time, so it cannot share the static slices. Data
        // stays on disk: a 4G tmpfs would compete with the other servers for memory.
        Engine::Mssql => {
            args.extend(strings(&[
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
                MSSQL_IMAGE,
            ]));
            return args;
        }
    };
    args.extend(strings(options));
    args.push(engine.image());
    args.extend(strings(command));
    args
}

/// The `exec` arguments after the runtime for one readiness probe: a real login over TCP on
/// the server's own port, which an entrypoint's socket-only init server never answers.
pub fn ready_args(engine: Engine, owner: u32) -> Vec<String> {
    let name = container_name(engine, owner);
    let probe: Vec<&str> = match engine {
        Engine::Postgres => {
            return strings(&[
                "exec",
                "-e",
                "PGPASSWORD=dbento",
                &name,
                "psql",
                "-h",
                "127.0.0.1",
                "-U",
                "postgres",
                "-d",
                "postgres",
                "-Atc",
                "SELECT 1",
            ]);
        }
        Engine::Mysql | Engine::Mariadb => vec![
            engine.name(),
            "-h127.0.0.1",
            "--protocol=TCP",
            "-uroot",
            "-pdbento",
            "-e",
            "SELECT 1",
        ],
        Engine::Mongo => vec![
            "mongosh",
            "--quiet",
            "--host",
            "127.0.0.1",
            "--port",
            "27018",
            "-u",
            "root",
            "-p",
            "dbento",
            "--authenticationDatabase",
            "admin",
            "--eval",
            "db.adminCommand({ping: 1})",
        ],
        Engine::Redis => vec!["valkey-cli", "ping"],
        Engine::Dynamo => vec![
            "python",
            "-c",
            "import urllib.request; urllib.request.urlopen('http://127.0.0.1:5000/moto-api/')",
        ],
        Engine::Surreal => vec![
            "/surreal",
            "is-ready",
            "--endpoint",
            "http://127.0.0.1:8000",
        ],
        Engine::Mssql => MSSQL_READY_PROBE.to_vec(),
    };
    let mut args = strings(&["exec", &name]);
    args.extend(strings(&probe));
    args
}

/// The `export` line for one server, from the output of `<runtime> port <name> <port>`.
pub fn export_line(engine: Engine, port_output: &str) -> Result<String> {
    let port = parse_port(port_output)?;
    let url = match engine {
        Engine::Postgres => format!("postgres://postgres:dbento@127.0.0.1:{port}/postgres"),
        Engine::Mysql | Engine::Mariadb => format!("mysql://root:dbento@127.0.0.1:{port}/"),
        Engine::Mongo => format!("mongodb://root:dbento@127.0.0.1:{port}"),
        Engine::Redis => format!("redis://127.0.0.1:{port}/0"),
        Engine::Dynamo => format!("http://127.0.0.1:{port}"),
        Engine::Surreal => format!("surreal://root:dbento@127.0.0.1:{port}"),
        Engine::Mssql => {
            return ms_export_lines(port_output)?
                .pop()
                .context("no SQL Server export line");
        }
    };
    Ok(format!("export {}={url}", engine.url_var()))
}

/// The root-level logins the Surreal tests sign in as, with their roles; the testkit's
/// per-test seed defines them only in container mode, so a shared server needs them here.
pub const SURREAL_ROOT_LOGINS: [(&str, &str); 3] =
    [("ro", "OWNER"), ("re", "EDITOR"), ("rv", "VIEWER")];

/// `DEFINE USER OVERWRITE ... ON ROOT` for each of [`SURREAL_ROOT_LOGINS`], password `dbento`.
pub fn surreal_logins_sql() -> String {
    SURREAL_ROOT_LOGINS
        .iter()
        .map(|(user, role)| {
            format!("DEFINE USER OVERWRITE {user} ON ROOT PASSWORD 'dbento' ROLES {role};")
        })
        .collect()
}

/// The `exec` arguments after the runtime for the Surreal CLI as `user`, reading SQL on stdin.
pub fn surreal_sql_args(owner: u32, user: &str) -> Vec<String> {
    strings(&[
        "exec",
        "-i",
        &container_name(Engine::Surreal, owner),
        "/surreal",
        "sql",
        "--endpoint",
        "http://127.0.0.1:8000",
        "-u",
        user,
        "-p",
        "dbento",
        "--hide-welcome",
        "--json",
    ])
}

/// The engines still to start: those whose URL variable is unset or empty, as the testkit
/// treats an empty variable as "start a container". SQL Server joins last, and only when its
/// licence variable is exactly `Y`, since starting it accepts that licence.
pub fn pending(env: impl Fn(&str) -> Option<String>) -> Vec<Engine> {
    let unset = |e: &Engine| env(e.url_var()).is_none_or(|v| v.is_empty());
    let licensed = mssql_eula(&env).is_ok();
    ENGINES
        .into_iter()
        .chain(licensed.then_some(Engine::Mssql))
        .filter(unset)
        .collect()
}

/// The runtime to drive when none is given: podman when `DOCKER_HOST` points at a podman
/// socket, as the testkit's testcontainers sees it, else docker.
pub fn default_runtime(docker_host: Option<&str>) -> &'static str {
    match docker_host {
        Some(host) if host.contains("podman") => "podman",
        _ => "docker",
    }
}

/// How long every pending server together gets to answer its first login.
const READY_DEADLINE: Duration = Duration::from_secs(180);

/// Starts every pending engine, waits until each answers a login, and returns its export lines
/// in `pending` order, SQL Server's followed by the stock-certificate flag. On any failure
/// removes every container it started and returns the error.
pub fn up(runtime: &str, owner: u32, env: impl Fn(&str) -> Option<String>) -> Result<Vec<String>> {
    let mut started = Vec::new();
    start_all(runtime, owner, &pending(env), &mut started)
        .map_err(|e| remove_started(runtime, &started, e, remove_args))
}

/// Starts each engine detached, then probes them all against one deadline so they initialise
/// side by side. Each name goes into `started` before its attempt, so a half-start is removed.
fn start_all(
    runtime: &str,
    owner: u32,
    engines: &[Engine],
    started: &mut Vec<String>,
) -> Result<Vec<String>> {
    for &e in engines {
        let name = container_name(e, owner);
        eprintln!("starting {name}");
        // Clear a leftover from an earlier run with this owner; failure means there was none.
        let _ = quiet(runtime, &remove_args(&[&name]));
        started.push(name);
        capture(runtime, &as_strs(&run_args(e, owner)))
            .with_context(|| format!("starting {}", e.name()))?;
    }
    let deadline = Instant::now() + READY_DEADLINE;
    for &e in engines {
        let probe = ready_args(e, owner);
        while !quiet(runtime, &as_strs(&probe)).with_context(|| format!("probing {}", e.name()))? {
            ensure!(
                Instant::now() < deadline,
                "{} did not answer a login within {} s",
                e.name(),
                READY_DEADLINE.as_secs()
            );
            std::thread::sleep(Duration::from_millis(500));
        }
    }
    if engines.contains(&Engine::Surreal) {
        define_surreal_logins(runtime, owner)?;
    }
    // The shared SQL Server keeps the image's self-signed certificate, which the TLS tests
    // need to be told about.
    let mut lines = Vec::new();
    for &e in engines {
        lines.push(
            export_of(runtime, e, owner)
                .with_context(|| format!("finding the port of {}", e.name()))?,
        );
        if e == Engine::Mssql {
            lines.push("export DBENTO_MSSQL_STOCK_CERT=1".to_owned());
        }
    }
    Ok(lines)
}

/// Defines the Surreal root logins as root, then signs in as each. The CLI exits 0 when a
/// statement fails but 1 when a login does, so the sign-ins are what proves the define landed.
fn define_surreal_logins(runtime: &str, owner: u32) -> Result<()> {
    let logins = surreal_logins_sql();
    let attempts = std::iter::once(("root", logins.as_str())).chain(
        SURREAL_ROOT_LOGINS
            .iter()
            .map(|(user, _)| (*user, "RETURN 1;")),
    );
    for (user, sql) in attempts {
        let args = surreal_sql_args(owner, user);
        ensure!(
            feed(runtime, &as_strs(&args), sql)
                .with_context(|| format!("running SurrealDB SQL as {user}"))?,
            "SurrealDB refused the login {user}"
        );
    }
    Ok(())
}

/// The export line for a running server, from the host port the runtime mapped.
fn export_of(runtime: &str, engine: Engine, owner: u32) -> Result<String> {
    let port = engine.port().to_string();
    let out = capture(runtime, &["port", &container_name(engine, owner), &port])?;
    export_line(engine, &out)
}

/// Force-removes, with volumes, every container labelled dbento.test-servers=<owner>.
pub fn down(runtime: &str, owner: u32) -> Result<()> {
    let listing = capture(
        runtime,
        &["ps", "-a", "-q", "--filter", &owner_filter(owner)],
    )?;
    let ids: Vec<&str> = listing.split_whitespace().collect();
    if !ids.is_empty() {
        capture(runtime, &remove_args(&ids))?;
    }
    Ok(())
}

/// Runs the runtime with its output discarded and reports whether it succeeded.
fn quiet(runtime: &str, args: &[&str]) -> Result<bool> {
    let status = Command::new(runtime)
        .args(args)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .with_context(|| format!("running {runtime}"))?;
    Ok(status.success())
}

/// Runs the runtime with `input` on its stdin and its output discarded, and reports whether it
/// succeeded. Stdin is always piped: an inherited terminal would leave a REPL waiting.
fn feed(runtime: &str, args: &[&str], input: &str) -> Result<bool> {
    let mut child = Command::new(runtime)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("running {runtime}"))?;
    // A child that exits without reading closes the pipe early; its exit status decides.
    let written = child
        .stdin
        .take()
        .map_or(Ok(()), |mut stdin| stdin.write_all(input.as_bytes()));
    let status = child
        .wait()
        .with_context(|| format!("waiting for {runtime}"))?;
    match written {
        Err(e) if e.kind() != ErrorKind::BrokenPipe => {
            Err(e).with_context(|| format!("writing to {runtime}"))
        }
        Ok(()) | Err(_) => Ok(status.success()),
    }
}

fn as_strs(args: &[String]) -> Vec<&str> {
    args.iter().map(String::as_str).collect()
}
