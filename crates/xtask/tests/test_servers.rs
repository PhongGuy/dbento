//! `cargo xtask test-servers`: the shared servers' arguments, URLs and runtime, and their start
//! and removal.
use std::process::Command;

use dbento_testkit::{MARIADB_TAG, MONGO_TAG, MOTO_TAG, MYSQL_TAG, PG_TAG, VALKEY_TAG};
use xtask::test_servers::{
    ENGINES, Engine, container_name, default_runtime, export_line, owner_filter, pending,
    ready_args, run_args,
};
use xtask::test_servers::{down, up};

/// The arguments every server shares, for owner 4242, then `rest`.
fn with(engine: &str, port: u16, rest: &[&str]) -> Vec<String> {
    let mut args: Vec<String> = [
        "run",
        "-d",
        "--name",
        &format!("dbento-test-{engine}-4242"),
        "--label",
        "dbento.role=test",
        "--label",
        "dbento.pid=4242",
        "--label",
        "dbento.test-servers=4242",
        "-p",
        &format!("127.0.0.1::{port}"),
    ]
    .map(String::from)
    .to_vec();
    args.extend(rest.iter().map(|s| (*s).to_owned()));
    args
}

#[test]
fn each_server_is_labelled_for_its_owner_and_tuned_as_measured() {
    assert_eq!(
        run_args(Engine::Postgres, 4242),
        with(
            "postgres",
            5432,
            &[
                "--tmpfs",
                "/var/lib/postgresql/data",
                "-e",
                "POSTGRES_PASSWORD=dbento",
                "docker.io/library/postgres:17-alpine",
                "-c",
                "fsync=off",
                "-c",
                "full_page_writes=off",
                "-c",
                "synchronous_commit=off",
                "-c",
                "max_connections=500",
            ]
        )
    );
    assert_eq!(
        run_args(Engine::Mysql, 4242),
        with(
            "mysql",
            3306,
            &[
                "--tmpfs",
                "/var/lib/mysql",
                "-e",
                "MYSQL_ROOT_PASSWORD=dbento",
                "docker.io/library/mysql:8.4",
            ]
        )
    );
    assert_eq!(
        run_args(Engine::Mariadb, 4242),
        with(
            "mariadb",
            3306,
            &[
                "--tmpfs",
                "/var/lib/mysql",
                "-e",
                "MARIADB_ROOT_PASSWORD=dbento",
                "docker.io/library/mariadb:11.4",
            ]
        )
    );
    assert_eq!(
        run_args(Engine::Mongo, 4242),
        with(
            "mongo",
            27018,
            &[
                "--tmpfs",
                "/data/db:size=2g,mode=1777",
                "-e",
                "MONGO_INITDB_ROOT_USERNAME=root",
                "-e",
                "MONGO_INITDB_ROOT_PASSWORD=dbento",
                "-e",
                "GLIBC_TUNABLES=glibc.pthread.rseq=1",
                "docker.io/library/mongo:8.0",
                "--wiredTigerCacheSizeGB",
                "0.5",
                "--port",
                "27018",
            ]
        )
    );
    assert_eq!(
        run_args(Engine::Redis, 4242),
        with(
            "redis",
            6379,
            &[
                "docker.io/valkey/valkey:9.0.7-alpine",
                "valkey-server",
                "--save",
                "",
                "--appendonly",
                "no",
            ]
        )
    );
    assert_eq!(
        run_args(Engine::Dynamo, 4242),
        with("dynamo", 5000, &["docker.io/motoserver/moto:5.2.5"])
    );
}

#[test]
fn the_images_are_the_ones_the_testkit_starts() {
    let tags: Vec<&str> = ENGINES.iter().map(|e| e.tag()).collect();
    assert_eq!(
        tags,
        [
            PG_TAG,
            MYSQL_TAG,
            MARIADB_TAG,
            MONGO_TAG,
            VALKEY_TAG,
            MOTO_TAG
        ]
    );
}

#[test]
fn readiness_is_a_login_over_tcp_on_the_servers_own_port() {
    assert_eq!(
        ready_args(Engine::Postgres, 4242),
        [
            "exec",
            "-e",
            "PGPASSWORD=dbento",
            "dbento-test-postgres-4242",
            "psql",
            "-h",
            "127.0.0.1",
            "-U",
            "postgres",
            "-d",
            "postgres",
            "-Atc",
            "SELECT 1",
        ]
    );
    assert_eq!(
        ready_args(Engine::Mongo, 4242),
        [
            "exec",
            "dbento-test-mongo-4242",
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
        ]
    );
    for e in ENGINES {
        let args = ready_args(e, 4242);
        assert_eq!(args[0], "exec");
        assert!(args.contains(&container_name(e, 4242)), "{args:?}");
    }
}

#[test]
fn each_export_line_is_a_url_the_testkit_parses() {
    let port = "127.0.0.1:41234\n[::]:41234\n";
    let lines: Vec<String> = ENGINES
        .iter()
        .map(|&e| export_line(e, port).unwrap())
        .collect();
    assert_eq!(
        lines,
        [
            "export DBENTO_PG_URL=postgres://postgres:dbento@127.0.0.1:41234/postgres",
            "export DBENTO_MYSQL_URL=mysql://root:dbento@127.0.0.1:41234/",
            "export DBENTO_MARIADB_URL=mysql://root:dbento@127.0.0.1:41234/",
            "export DBENTO_MONGO_URL=mongodb://root:dbento@127.0.0.1:41234",
            "export DBENTO_REDIS_URL=redis://127.0.0.1:41234/0",
            "export DBENTO_DYNAMO_URL=http://127.0.0.1:41234",
        ]
    );
    let url = |e: Engine| {
        let line = export_line(e, port).unwrap();
        line.split_once('=').unwrap().1.to_owned()
    };
    assert_eq!(
        dbento_testkit::mg_url(&url(Engine::Mongo)).unwrap(),
        (
            "127.0.0.1".to_owned(),
            41234,
            "root".to_owned(),
            "dbento".to_owned()
        )
    );
    assert_eq!(
        dbento_testkit::rd_url(&url(Engine::Redis)).unwrap(),
        ("127.0.0.1".to_owned(), 41234, None, None, 0)
    );
    assert!(export_line(Engine::Postgres, "").is_err());
}

#[test]
fn only_engines_whose_variable_is_unset_or_empty_are_started() {
    assert_eq!(pending(|_| None), ENGINES);
    let env = |k: &str| match k {
        "DBENTO_PG_URL" => Some("postgres://elsewhere/postgres".to_owned()),
        "DBENTO_REDIS_URL" => Some(String::new()),
        _ => None,
    };
    assert_eq!(
        pending(env),
        [
            Engine::Mysql,
            Engine::Mariadb,
            Engine::Mongo,
            Engine::Redis,
            Engine::Dynamo
        ]
    );
    let vars: Vec<&str> = ENGINES.iter().map(|e| e.url_var()).collect();
    assert_eq!(
        vars,
        [
            "DBENTO_PG_URL",
            "DBENTO_MYSQL_URL",
            "DBENTO_MARIADB_URL",
            "DBENTO_MONGO_URL",
            "DBENTO_REDIS_URL",
            "DBENTO_DYNAMO_URL",
        ]
    );
}

#[test]
fn the_runtime_follows_docker_host_like_the_testkit() {
    assert_eq!(
        default_runtime(Some("unix:///run/user/1000/podman/podman.sock")),
        "podman"
    );
    assert_eq!(
        default_runtime(Some("unix:///var/run/docker.sock")),
        "docker"
    );
    assert_eq!(default_runtime(Some("")), "docker");
    assert_eq!(default_runtime(None), "docker");
}

#[test]
fn names_and_the_removal_filter_carry_the_owner() {
    assert_eq!(container_name(Engine::Mongo, 7), "dbento-test-mongo-7");
    assert_eq!(owner_filter(7), "label=dbento.test-servers=7");
}

/// Every server's container name for owner 4242, in start order.
const ALL: [&str; 6] = [
    "dbento-test-postgres-4242",
    "dbento-test-mysql-4242",
    "dbento-test-mariadb-4242",
    "dbento-test-mongo-4242",
    "dbento-test-redis-4242",
    "dbento-test-dynamo-4242",
];

/// A fake runtime that logs each call to `calls.log` and answers like podman: `run` prints an id,
/// `port` 127.0.0.1:5555, `ps` two ids, the rest succeed silently. `arm` is a shell `case` arm
/// tried first, to make one call fail or answer differently.
#[cfg(unix)]
fn fake_runtime(dir: &std::path::Path, arm: &str) -> (String, std::path::PathBuf) {
    let log = dir.join("calls.log");
    let script = dir.join("fake-runtime");
    crate::fake_exe::write_executable(
        &script,
        &format!(
            "#!/bin/sh\necho \"$*\" >> '{log}'\ncase \"$*\" in\n{arm}\n\
             run*) echo fakeid ;;\n\
             port*) echo 127.0.0.1:5555 ;;\n\
             ps*) printf 'id1\\nid2\\n' ;;\n\
             esac\nexit 0\n",
            log = log.display()
        ),
    );
    (script.to_str().unwrap().to_owned(), log)
}

#[cfg(unix)]
fn calls(log: &std::path::Path) -> Vec<String> {
    std::fs::read_to_string(log)
        .unwrap_or_default()
        .lines()
        .map(str::to_owned)
        .collect()
}

#[cfg(unix)]
#[test]
fn up_starts_every_unset_engine_and_returns_its_export_line() {
    let dir = tempfile::tempdir().unwrap();
    let (rt, log) = fake_runtime(dir.path(), "");
    let lines = up(&rt, 4242, |_| None).unwrap();
    let want: Vec<String> = ENGINES
        .iter()
        .map(|&e| export_line(e, "127.0.0.1:5555").unwrap())
        .collect();
    assert_eq!(lines, want);
    let calls = calls(&log);
    for name in ALL {
        assert!(
            calls
                .iter()
                .any(|c| c.starts_with("run -d --name ") && c.contains(name)),
            "{name} never ran: {calls:?}"
        );
        assert!(
            calls
                .iter()
                .any(|c| c.starts_with("exec") && c.contains(name)),
            "{name} never probed: {calls:?}"
        );
    }
    // Every server starts before the first probe, so they initialise together.
    let last_run = calls.iter().rposition(|c| c.starts_with("run ")).unwrap();
    let first_exec = calls.iter().position(|c| c.starts_with("exec ")).unwrap();
    assert!(last_run < first_exec, "{calls:?}");
    assert!(
        calls[last_run..].iter().all(|c| !c.starts_with("rm ")),
        "{calls:?}"
    );
}

#[cfg(unix)]
#[test]
fn an_engine_whose_variable_is_set_is_left_alone() {
    let dir = tempfile::tempdir().unwrap();
    let (rt, log) = fake_runtime(dir.path(), "");
    let env = |k: &str| (k == "DBENTO_PG_URL").then(|| "postgres://elsewhere/postgres".to_owned());
    let lines = up(&rt, 4242, env).unwrap();
    assert_eq!(lines.len(), 5, "{lines:?}");
    assert!(
        lines
            .iter()
            .all(|l| !l.starts_with("export DBENTO_PG_URL=")),
        "{lines:?}"
    );
    let calls = calls(&log);
    assert!(
        calls.iter().all(|c| !c.contains("dbento-test-postgres")),
        "{calls:?}"
    );
}

#[cfg(unix)]
#[test]
fn a_failed_start_removes_every_server_started_so_far() {
    let dir = tempfile::tempdir().unwrap();
    let (rt, log) = fake_runtime(
        dir.path(),
        "run*dbento-test-mariadb-4242*) echo boom >&2; exit 1 ;;",
    );
    let err = up(&rt, 4242, |_| None).unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("boom") && msg.contains("mariadb"), "{msg}");
    assert_eq!(
        calls(&log).last().map(String::as_str),
        Some("rm -f -v dbento-test-postgres-4242 dbento-test-mysql-4242 dbento-test-mariadb-4242")
    );
}

#[cfg(unix)]
#[test]
fn a_failure_after_every_start_removes_all_six() {
    let dir = tempfile::tempdir().unwrap();
    let (rt, log) = fake_runtime(
        dir.path(),
        "port\\ dbento-test-dynamo-4242*) echo boom >&2; exit 1 ;;",
    );
    assert!(up(&rt, 4242, |_| None).is_err());
    let want = format!("rm -f -v {}", ALL.join(" "));
    assert_eq!(calls(&log).last(), Some(&want));
}

#[cfg(unix)]
#[test]
fn down_removes_every_container_the_owner_labelled() {
    let dir = tempfile::tempdir().unwrap();
    let (rt, log) = fake_runtime(dir.path(), "");
    down(&rt, 4242).unwrap();
    assert_eq!(
        calls(&log),
        [
            "ps -a -q --filter label=dbento.test-servers=4242",
            "rm -f -v id1 id2"
        ]
    );
}

#[cfg(unix)]
#[test]
fn down_with_nothing_to_remove_succeeds() {
    let dir = tempfile::tempdir().unwrap();
    let (rt, log) = fake_runtime(dir.path(), "ps*) ;;");
    down(&rt, 4242).unwrap();
    assert_eq!(
        calls(&log),
        ["ps -a -q --filter label=dbento.test-servers=4242"]
    );
}

#[cfg(unix)]
#[test]
fn the_cli_removes_by_owner_and_starts_nothing_already_set() {
    let dir = tempfile::tempdir().unwrap();
    let (rt, log) = fake_runtime(dir.path(), "");
    let out = Command::new(env!("CARGO_BIN_EXE_xtask"))
        .args(["test-servers", "down", "--owner", "4242", "--runtime", &rt])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(calls(&log).len(), 2);

    std::fs::remove_file(&log).unwrap();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_xtask"));
    cmd.args(["test-servers", "up", "--owner", "4242", "--runtime", &rt]);
    for e in ENGINES {
        cmd.env(e.url_var(), "set-elsewhere");
    }
    let out = cmd.output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(String::from_utf8_lossy(&out.stdout), "");
    assert_eq!(calls(&log), Vec::<String>::new());
}

#[cfg(unix)]
#[test]
fn a_cli_start_failure_exits_non_zero_and_prints_no_exports() {
    let dir = tempfile::tempdir().unwrap();
    let (rt, _log) = fake_runtime(
        dir.path(),
        "run*dbento-test-mysql-4242*) echo boom >&2; exit 1 ;;",
    );
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_xtask"));
    cmd.args(["test-servers", "up", "--owner", "4242", "--runtime", &rt]);
    for e in ENGINES {
        cmd.env_remove(e.url_var());
    }
    let out = cmd.output().unwrap();
    assert!(!out.status.success());
    assert_eq!(String::from_utf8_lossy(&out.stdout), "");
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("boom"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}
