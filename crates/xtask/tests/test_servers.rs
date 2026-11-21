//! `cargo xtask test-servers`: the shared servers' arguments, URLs and runtime, and their start
//! and removal.
use std::process::Command;

use dbento_testkit::{
    MARIADB_TAG, MONGO_TAG, MOTO_TAG, MYSQL_TAG, PG_TAG, SURREAL_TAG, VALKEY_TAG,
};
use xtask::test_servers::{
    ENGINES, Engine, SURREAL_ROOT_LOGINS, container_name, default_runtime, export_line,
    owner_filter, pending, ready_args, run_args, surreal_logins_sql, surreal_sql_args,
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
                "--quiet",
                "--setParameter",
                "enableDetailedConnectionHealthMetricLogLines=false",
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
    assert_eq!(
        run_args(Engine::Surreal, 4242),
        with(
            "surreal",
            8000,
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
                "docker.io/surrealdb/surrealdb:v3.3.5",
                "start",
            ]
        )
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
            MOTO_TAG,
            SURREAL_TAG
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
    assert_eq!(
        ready_args(Engine::Surreal, 4242),
        [
            "exec",
            "dbento-test-surreal-4242",
            "/surreal",
            "is-ready",
            "--endpoint",
            "http://127.0.0.1:8000",
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
            "export DBENTO_SURREAL_URL=surreal://root:dbento@127.0.0.1:41234",
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
    assert_eq!(
        dbento_testkit::sr_url(&url(Engine::Surreal)).unwrap(),
        (
            "127.0.0.1".to_owned(),
            41234,
            "root".to_owned(),
            "dbento".to_owned()
        )
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
            Engine::Dynamo,
            Engine::Surreal
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
            "DBENTO_SURREAL_URL",
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
const ALL: [&str; 7] = [
    "dbento-test-postgres-4242",
    "dbento-test-mysql-4242",
    "dbento-test-mariadb-4242",
    "dbento-test-mongo-4242",
    "dbento-test-redis-4242",
    "dbento-test-dynamo-4242",
    "dbento-test-surreal-4242",
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
    assert_eq!(lines.len(), 6, "{lines:?}");
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
fn a_failure_after_every_start_removes_all_seven() {
    let dir = tempfile::tempdir().unwrap();
    let (rt, log) = fake_runtime(
        dir.path(),
        "port\\ dbento-test-surreal-4242*) echo boom >&2; exit 1 ;;",
    );
    assert!(up(&rt, 4242, |_| None).is_err());
    let want = format!("rm -f -v {}", ALL.join(" "));
    assert_eq!(calls(&log).last(), Some(&want));
}

#[test]
fn the_surreal_logins_are_the_ones_the_testkit_seeds_against() {
    assert_eq!(
        surreal_logins_sql(),
        dbento_testkit::sr_root_logins_sql("dbento")
    );
    let users: Vec<&str> = SURREAL_ROOT_LOGINS.iter().map(|(u, _)| *u).collect();
    let kit: Vec<&str> = dbento_testkit::SR_LOGINS
        .iter()
        .filter(|(_, level, _)| *level == "root")
        .map(|(u, _, _)| *u)
        .collect();
    assert_eq!(users, kit);
    assert_eq!(
        surreal_sql_args(4242, "ro"),
        [
            "exec",
            "-i",
            "dbento-test-surreal-4242",
            "/surreal",
            "sql",
            "--endpoint",
            "http://127.0.0.1:8000",
            "-u",
            "ro",
            "-p",
            "dbento",
            "--hide-welcome",
            "--json",
        ]
    );
}

#[cfg(unix)]
#[test]
fn up_defines_the_surreal_root_logins_after_readiness_then_logs_in_as_each() {
    let dir = tempfile::tempdir().unwrap();
    let (rt, log) = fake_runtime(dir.path(), "");
    up(&rt, 4242, |_| None).unwrap();
    let calls = calls(&log);
    let at = |user: &str| {
        let want = surreal_sql_args(4242, user).join(" ");
        calls
            .iter()
            .position(|c| *c == want)
            .unwrap_or_else(|| panic!("no sql as {user}: {calls:?}"))
    };
    let ready = calls
        .iter()
        .position(|c| c.starts_with("exec dbento-test-surreal-4242 /surreal is-ready"))
        .expect("Surreal never probed");
    assert!(ready < at("root"), "{calls:?}");
    assert!(
        at("root") < at("ro") && at("ro") < at("re") && at("re") < at("rv"),
        "{calls:?}"
    );
}

#[cfg(unix)]
#[test]
fn a_surreal_login_that_fails_removes_every_server() {
    let dir = tempfile::tempdir().unwrap();
    let (rt, log) = fake_runtime(dir.path(), "exec\\ -i*-u\\ rv\\ *) exit 1 ;;");
    let err = up(&rt, 4242, |_| None).unwrap_err();
    let msg = format!("{err:#}");
    assert!(msg.contains("rv"), "{msg}");
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

/// Set in the child of the live test; the child logs in to every shared server through the
/// testkit, from the host. Without it the child test does nothing.
const SERVERS_CHILD: &str = "DBENTO_TEST_SERVERS_CHILD";

/// The port of a test URL in any of the seven forms.
fn port_of(url: &str) -> u16 {
    let rest = url.split_once("://").expect("a scheme").1;
    let server = rest.rsplit_once('@').map_or(rest, |(_, s)| s);
    let address = server.split('/').next().unwrap_or_default();
    address
        .rsplit_once(':')
        .expect("a port")
        .1
        .parse()
        .expect("a numeric port")
}

/// Names of containers carrying all three of the owner's labels, sorted.
fn labelled_names(runtime: &str, owner: u32) -> Vec<String> {
    let out = Command::new(runtime)
        .args([
            "ps",
            "-a",
            "--filter",
            &format!("label=dbento.test-servers={owner}"),
            "--filter",
            &format!("label=dbento.pid={owner}"),
            "--filter",
            "label=dbento.role=test",
            "--format",
            "{{.Names}}",
        ])
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let mut names: Vec<String> = String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::to_owned)
        .collect();
    names.sort();
    names
}

/// Removes the test's servers however it ends, so a failed assertion leaves nothing running.
struct Owned {
    runtime: &'static str,
    owner: u32,
}

impl Drop for Owned {
    fn drop(&mut self) {
        let _ = down(self.runtime, self.owner);
    }
}

#[tokio::test]
async fn shared_servers_child() {
    if std::env::var_os(SERVERS_CHILD).is_none() {
        return;
    }
    let var = |k: &str| std::env::var(k).unwrap_or_else(|_| panic!("{k} is not set"));
    let pg = dbento_testkit::pg_db().await;
    assert_eq!(pg.container_id(), None);
    assert_eq!(pg.port(), port_of(&var("DBENTO_PG_URL")));
    assert_eq!(pg.scalar("SELECT 'ok'").await.as_deref(), Some("ok"));
    let my = dbento_testkit::mysql_db().await;
    assert_eq!(my.port(), port_of(&var("DBENTO_MYSQL_URL")));
    assert_eq!(my.scalar("SELECT 'ok'").await.as_deref(), Some("ok"));
    let maria = dbento_testkit::mariadb_db().await;
    assert_eq!(maria.port(), port_of(&var("DBENTO_MARIADB_URL")));
    assert_eq!(maria.scalar("SELECT 'ok'").await.as_deref(), Some("ok"));
    let mg = dbento_testkit::mongo_db().await;
    assert_eq!(mg.port(), port_of(&var("DBENTO_MONGO_URL")));
    assert_eq!(mg.count("many").await, 10_000);
    let rd = dbento_testkit::redis_db().await;
    assert_eq!(rd.port(), port_of(&var("DBENTO_REDIS_URL")));
    assert_eq!(
        rd.int(&["DBSIZE"]).await,
        i64::try_from(dbento_testkit::SEED_KEYS).unwrap()
    );
    let dy = dbento_testkit::dynamo_db().await;
    assert_eq!(dy.endpoint(), var("DBENTO_DYNAMO_URL"));
    assert_eq!(dy.count("orders").await, dbento_testkit::ORDERS);
    let sr = dbento_testkit::surreal_db().await;
    assert_eq!(sr.port(), port_of(&var("DBENTO_SURREAL_URL")));
    assert_eq!(sr.scalar("RETURN 7").await, 7);
}

#[test]
fn up_starts_every_labelled_server_the_testkit_reaches_and_down_removes_them() {
    let runtime = default_runtime(std::env::var("DOCKER_HOST").ok().as_deref());
    let owner = std::process::id();
    let owned = Owned { runtime, owner };
    // The caller's URL variables are ignored, so a run on shared servers still tests a full start.
    let lines = up(runtime, owner, |_| None).unwrap();
    let mut want: Vec<String> = ENGINES.iter().map(|&e| container_name(e, owner)).collect();
    want.sort();
    assert_eq!(labelled_names(runtime, owner), want);
    // The logins exist before any test runs, defined by `up` and not by the testkit.
    for (user, _) in SURREAL_ROOT_LOGINS {
        let mut child = Command::new(runtime)
            .args(surreal_sql_args(owner, user))
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        std::io::Write::write_all(child.stdin.as_mut().unwrap(), b"RETURN 1;").unwrap();
        drop(child.stdin.take());
        assert!(child.wait().unwrap().success(), "{user} cannot log in");
    }

    let exports = lines.iter().map(|l| {
        let (k, v) = l
            .strip_prefix("export ")
            .and_then(|kv| kv.split_once('='))
            .unwrap_or_else(|| panic!("not an export line: {l}"));
        (k.to_owned(), v.to_owned())
    });
    let out = Command::new(std::env::current_exe().unwrap())
        .args([
            "test_servers::shared_servers_child",
            "--exact",
            "--nocapture",
        ])
        .envs(exports)
        .env(SERVERS_CHILD, "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains("1 passed"),
        "{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );

    down(runtime, owner).unwrap();
    assert_eq!(labelled_names(runtime, owner), Vec::<String>::new());
    drop(owned);
}
