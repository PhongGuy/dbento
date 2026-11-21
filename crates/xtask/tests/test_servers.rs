//! `cargo xtask test-servers`: the shared servers' arguments, URLs and runtime, and their start
//! and removal.
use dbento_testkit::{MARIADB_TAG, MONGO_TAG, MOTO_TAG, MYSQL_TAG, PG_TAG, VALKEY_TAG};
use xtask::test_servers::{
    ENGINES, Engine, container_name, default_runtime, export_line, owner_filter, pending,
    ready_args, run_args,
};

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
