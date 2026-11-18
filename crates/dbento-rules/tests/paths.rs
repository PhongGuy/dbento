use crate::common::RULES;
use potluck_kind::Code;

const DATABASE: [&str; 17] = [
    ".db",
    ".sqlite",
    ".sqlite3",
    ".duckdb",
    ".db-wal",
    ".db-shm",
    ".db-journal",
    ".sqlite-wal",
    ".sqlite-shm",
    ".sqlite-journal",
    ".sqlite3-wal",
    ".sqlite3-shm",
    ".sqlite3-journal",
    ".duckdb-wal",
    ".duckdb-shm",
    ".duckdb-journal",
    ".duckdb.wal",
];
const KEY: [&str; 5] = [".key", ".p12", ".pfx", ".keychain", ".keychain-db"];

fn refused(path: &str) -> Option<(&'static str, Code, String, Option<String>)> {
    RULES.path_rule(path).map(|f| {
        (
            f.rule(),
            f.code(),
            f.path().to_owned(),
            f.detail().map(str::to_owned),
        )
    })
}

fn assert_refused_everywhere(suffixes: &[&str], rule: &'static str) {
    for suffix in suffixes {
        for path in [
            format!("app{suffix}"),
            format!("data/nested/app{suffix}"),
            format!("APP{}", suffix.to_ascii_uppercase()),
        ] {
            assert_eq!(
                refused(&path),
                Some((rule, Code::EnvFile, path.clone(), None)),
                "{path}"
            );
        }
    }
}

#[test]
fn every_database_suffix_is_refused_in_any_case_at_any_depth() {
    assert_refused_everywhere(&DATABASE, "database_file");
}

#[test]
fn every_key_suffix_is_refused_in_any_case_at_any_depth() {
    assert_refused_everywhere(&KEY, "key_file");
}

#[test]
fn near_misses_and_pem_pass() {
    for path in [
        "app.db.sql",
        "app.dbx",
        "app.sqlite.bak",
        "app.db-walx",
        "app.db.wal",
        "app.wal",
        "data.db/readme.md",
        "db",
        "ca.pem",
        "certs/server.pem",
        "monkey",
        "app.keys",
        "login.keychain-dbx",
        "dbento.toml",
        "queries/a/q.sql",
    ] {
        assert_eq!(refused(path), None, "{path}");
    }
}

#[test]
fn reads_only_dbento_toml_by_basename_ignoring_ascii_case() {
    for path in [
        "dbento.toml",
        "DBENTO.TOML",
        "team/Dbento.toml",
        "a/b/dbento.TOML",
    ] {
        assert!(RULES.reads(path), "{path}");
    }
    for path in [
        "dbento.toml.bak",
        "my-dbento.toml",
        "dbento.toml/x",
        "dbento.yml",
        "queries/q.sql",
        "dbento",
    ] {
        assert!(!RULES.reads(path), "{path}");
    }
}
