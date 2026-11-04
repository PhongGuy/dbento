use xtask::db::*;

#[test]
fn versions_parse_sorted_and_unique() {
    assert_eq!(parse_versions("18,14,18").unwrap(), vec![14, 18]);
    assert_eq!(parse_versions(" 15 , 16 ").unwrap(), vec![15, 16]);
    assert_eq!(
        parse_versions(DEFAULT_VERSIONS).unwrap(),
        vec![14, 15, 16, 17, 18]
    );
    for bad in ["", "13", "abc", "14,,15", "100"] {
        assert!(parse_versions(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn run_arguments_label_the_container_and_keep_the_data_on_tmpfs() {
    let expected = [
        "run",
        "-d",
        "--name",
        "dbento-pg-17",
        "--label",
        "dbento.role=matrix",
        "--label",
        "dbento.version=17",
        "--tmpfs",
        "/var/lib/postgresql/data",
        "-e",
        "POSTGRES_PASSWORD=dbento",
        "-p",
        "127.0.0.1::5432",
        "docker.io/library/postgres:17-alpine",
        "-c",
        "fsync=off",
        "-c",
        "full_page_writes=off",
        "-c",
        "synchronous_commit=off",
    ];
    assert_eq!(run_args(17), expected);
    let v18 = run_args(18);
    assert!(
        v18.windows(2)
            .any(|w| w == ["--tmpfs", "/var/lib/postgresql"]),
        "{v18:?}"
    );
    assert!(v18.contains(&"docker.io/library/postgres:18-alpine".to_owned()));
}

#[test]
fn the_export_lines_name_the_version_the_port_and_the_database() {
    assert_eq!(
        export_lines(17, "127.0.0.1:43461\n").unwrap(),
        [
            "export DBENTO_PG_17_URL=postgres://postgres:dbento@127.0.0.1:43461/postgres",
            "export DBENTO_PG_17_CHINOOK_URL=postgres://postgres:dbento@127.0.0.1:43461/chinook",
        ]
    );
    assert!(export_lines(17, "").is_err());
    assert!(export_lines(17, "127.0.0.1:notaport").is_err());
}

#[test]
fn orphans_are_test_containers_whose_process_is_gone() {
    let listing = "aaa dbento.pid=10,dbento.role=test,org.testcontainers.managed-by=testcontainers\n\
                   bbb dbento.pid=20,dbento.role=test\nccc dbento.role=test\n\n";
    assert_eq!(orphans(listing, |pid| pid == 10), vec!["bbb".to_owned()]);
    assert!(orphans(listing, |_| true).is_empty());
}

#[test]
fn the_vendored_chinook_files_match_their_pinned_hashes() {
    let dir = xtask::workspace_root().join("datasets/chinook");
    let hash = |f: &str| sha256_hex(&std::fs::read(dir.join(f)).unwrap());
    assert_eq!(hash("Chinook_PostgreSql.sql"), CHINOOK_PG_SHA256);
    assert_eq!(hash("Chinook_Sqlite.sql"), CHINOOK_SQLITE_SHA256);
    let notice = std::fs::read_to_string(dir.join("LICENSE.md")).unwrap();
    assert!(notice.contains("Copyright (c) 2008-2024 Luis Rocha"));
}

#[test]
fn the_sqlite_chinook_builds_over_a_stale_file() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("chinook.sqlite");
    std::fs::write(&path, "stale").unwrap();
    build_sqlite_chinook(&path).unwrap();
    let c = rusqlite::Connection::open(&path).unwrap();
    let n: i64 = c
        .query_row("select count(*) from Track", [], |r| r.get(0))
        .unwrap();
    assert_eq!(n, 3503);
}

#[test]
fn removal_takes_the_containers_volumes_with_them() {
    assert_eq!(remove_args(&["a", "b"]), ["rm", "-f", "-v", "a", "b"]);
}

/// A fake runtime that logs each call and fails to start `dbento-pg-15`.
#[cfg(unix)]
#[test]
fn a_failed_up_removes_the_containers_it_started() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let log = dir.path().join("calls.log");
    let script = dir.path().join("fake-runtime");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\necho \"$*\" >> '{}'\ncase \"$*\" in\n\
             run*dbento-pg-15*) echo boom >&2; exit 1 ;;\n\
             run*) echo fakeid ;;\n\
             exec\\ -i*) cat > /dev/null ;;\n\
             port*) echo 127.0.0.1:5555 ;;\n\
             esac\nexit 0\n",
            log.display()
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let err = up(script.to_str().unwrap(), &[14, 15], &[]).unwrap_err();
    assert!(format!("{err:#}").contains("boom"), "{err:#}");
    let calls = std::fs::read_to_string(&log).unwrap();
    assert_eq!(
        calls.lines().last(),
        Some("rm -f -v dbento-pg-14 dbento-pg-15"),
        "{calls}"
    );
}

#[test]
fn the_port_comes_from_the_first_line() {
    assert_eq!(parse_port("127.0.0.1:43461\n[::]:43461\n").unwrap(), 43461);
    assert!(parse_port("").is_err());
    assert!(parse_port("127.0.0.1:x").is_err());
}

#[test]
fn a_dataset_export_names_version_label_and_database() {
    assert_eq!(
        dataset_export(17, 43461, "STRESS", "stress"),
        "export DBENTO_PG_17_STRESS_URL=postgres://postgres:dbento@127.0.0.1:43461/stress"
    );
    assert_eq!(
        dataset_export(14, 5, "AIRLINES", "demo"),
        "export DBENTO_PG_14_AIRLINES_URL=postgres://postgres:dbento@127.0.0.1:5/demo"
    );
}

#[test]
fn mysql_family_engines_and_versions_parse() {
    assert_eq!(parse_engine("postgres").unwrap(), None);
    assert_eq!(parse_engine("mariadb").unwrap(), Some(MyEngine::Mariadb));
    assert!(parse_engine("oracle").is_err());
    assert_eq!(parse_my_versions("9.7, 8.4,8.4").unwrap(), ["8.4", "9.7"]);
    for bad in ["", "8", "8.x", "8.4.1", ",8.4"] {
        assert!(parse_my_versions(bad).is_err(), "{bad:?}");
    }
    assert_eq!(my_label("10.11"), "1011");
}

#[test]
fn mysql_family_containers_are_labelled_and_on_tmpfs() {
    let expected = [
        "run",
        "-d",
        "--name",
        "dbento-mysql-8.4",
        "--label",
        "dbento.role=matrix",
        "--label",
        "dbento.version=mysql-8.4",
        "--tmpfs",
        "/var/lib/mysql",
        "-e",
        "MYSQL_ROOT_PASSWORD=dbento",
        "-p",
        "127.0.0.1::3306",
        "docker.io/library/mysql:8.4",
    ];
    assert_eq!(my_run_args(MyEngine::Mysql, "8.4"), expected);
    let maria = my_run_args(MyEngine::Mariadb, "11.4");
    assert_eq!(maria[3], "dbento-mariadb-11.4");
    assert!(maria.contains(&"MARIADB_ROOT_PASSWORD=dbento".to_owned()));
    assert_eq!(maria.last().unwrap(), "docker.io/library/mariadb:11.4");
}

#[test]
fn mysql_family_export_lines_name_the_matrix_variable() {
    assert_eq!(
        my_export_lines(MyEngine::Mariadb, "11.4", "127.0.0.1:41234\n", false).unwrap(),
        ["export DBENTO_MARIADB_114_URL=mysql://root:dbento@127.0.0.1:41234/"]
    );
    assert_eq!(
        my_export_lines(MyEngine::Mysql, "8.4", "127.0.0.1:41235\n", true).unwrap(),
        [
            "export DBENTO_MYSQL_84_URL=mysql://root:dbento@127.0.0.1:41235/",
            "export DBENTO_MYSQL_URL=mysql://root:dbento@127.0.0.1:41235/",
        ]
    );
}

#[test]
fn sql_server_needs_the_exact_licence_variable_before_anything_runs() {
    assert!(mssql_eula(|k| (k == "DBENTO_MSSQL_ACCEPT_EULA").then(|| "Y".to_owned())).is_ok());
    for value in [None, Some("y"), Some("yes"), Some("")] {
        let e = mssql_eula(|_| value.map(str::to_owned))
            .unwrap_err()
            .to_string();
        assert!(
            e.contains("DBENTO_MSSQL_ACCEPT_EULA=Y") && e.contains("licence"),
            "{e}"
        );
    }
    // A runtime that does not exist proves the refusal comes before any command.
    let e = up_mssql("/nonexistent/podman", |_| None)
        .unwrap_err()
        .to_string();
    assert!(e.contains("DBENTO_MSSQL_ACCEPT_EULA"), "{e}");
    assert!(
        parse_engine("oracle")
            .unwrap_err()
            .to_string()
            .contains("mssql")
    );
}

#[test]
fn the_sql_server_container_is_labelled_capped_and_on_tmpfs() {
    let expected = [
        "run",
        "-d",
        "--name",
        "dbento-mssql-2022",
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
        "MSSQL_SA_PASSWORD=DbentoPass2022",
        "-p",
        "127.0.0.1::1433",
        "mcr.microsoft.com/mssql/server:2022-CU27-ubuntu-22.04",
    ];
    assert_eq!(ms_run_args(), expected);
    assert_eq!(
        ms_export_lines("127.0.0.1:41433\n").unwrap(),
        ["export DBENTO_MSSQL_URL=mssql://sa:DbentoPass2022@127.0.0.1:41433"]
    );
    assert!(ms_export_lines("").is_err());
    assert_eq!(
        kill_args(&["dbento-mssql-2022"]),
        ["rm", "-f", "-t", "0", "-v", "dbento-mssql-2022"]
    );
}
