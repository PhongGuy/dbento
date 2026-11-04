//! MySQL and MariaDB Beekeeper connections through the binary, with and without secrets.
mod common;
use common::dbento;
use dbento_core::{Engine, Workspace};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Output;
use tempfile::TempDir;

const FIXTURES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../dbento-core/tests/fixtures/beekeeper"
);

/// `<tmp>/beekeeper-studio` built from mysql.sql with the fixture `.key`.
fn source() -> (TempDir, PathBuf) {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("beekeeper-studio");
    std::fs::create_dir(&dir).unwrap();
    let script = std::fs::read_to_string(Path::new(FIXTURES).join("mysql.sql")).unwrap();
    rusqlite::Connection::open(dir.join("app.db"))
        .unwrap()
        .execute_batch(&script)
        .unwrap();
    std::fs::copy(Path::new(FIXTURES).join(".key"), dir.join(".key")).unwrap();
    (tmp, dir)
}

fn import(ws: &Path, source: &Path, extra: &[&str]) -> Output {
    dbento()
        .env("DBENTO_TEST_NO_KEYCHAIN", "1")
        .arg("--workspace")
        .arg(ws)
        .args(["import", "beekeeper"])
        .arg(source)
        .args(["--format", "json"])
        .args(extra)
        .output()
        .unwrap()
}

fn json_of(out: &Output) -> serde_json::Value {
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stdout)))
}

const NEXT: [&str; 2] = ["dbento secret set shop-mysql", "dbento secret set maria-ro"];

#[test]
fn mysql_and_mariadb_import_as_engine_mysql_and_a_rerun_changes_nothing() {
    let (tmp, src) = source();
    let ws = tmp.path().join("ws");
    let out = import(&ws, &src, &[]);
    let doc = json_of(&out);
    assert_eq!(
        doc["connections"],
        json!([
            {"name": "shop-mysql", "from": "Shop MySQL", "action": "add"},
            {"name": "maria-ro", "from": "Maria (RO)", "action": "add"}
        ])
    );
    let skipped: Vec<(&str, &str)> = doc["skipped"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| (s["from"].as_str().unwrap(), s["reason"].as_str().unwrap()))
        .collect();
    assert_eq!(
        skipped,
        [
            ("Socket", "unix_socket"),
            ("TiDB", "unsupported_driver"),
            ("IAM", "unsupported_driver"),
            ("No user", "no_user")
        ]
    );
    // The keychain is disabled, so both saved passwords fail to store and stay a next step.
    assert_eq!(doc["secrets"]["failed"], json!(["shop-mysql", "maria-ro"]));
    assert_eq!(doc["next_steps"], json!(NEXT));
    let opened = Workspace::open(&ws).unwrap();
    let engines: Vec<Engine> = opened.connections().iter().map(|c| c.engine()).collect();
    assert_eq!(engines, [Engine::Mysql, Engine::Mysql]);
    let toml = std::fs::read_to_string(ws.join("dbento.toml")).unwrap();
    for text in [
        toml,
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    ] {
        assert!(!text.contains("fake-pw"), "{text}");
    }
    let again = json_of(&import(&ws, &src, &[]));
    for item in again["connections"].as_array().unwrap() {
        assert_eq!(item["action"], "unchanged", "{item}");
    }
}

#[test]
fn no_secrets_imports_the_same_connections_and_tries_no_keychain() {
    let (tmp, src) = source();
    let ws = tmp.path().join("ws");
    let doc = json_of(&import(&ws, &src, &["--no-secrets"]));
    assert_eq!(
        doc["secrets"],
        json!({"stored": [], "present": [], "failed": []})
    );
    assert_eq!(doc["next_steps"], json!(NEXT));
    assert_eq!(Workspace::open(&ws).unwrap().connections().len(), 2);
}
