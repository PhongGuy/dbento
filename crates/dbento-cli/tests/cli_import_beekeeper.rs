//! `dbento import beekeeper` on small synthetic data folders; the keychain is always disabled.
use crate::common::dbento;
use serde_json::json;
use std::path::Path;
use std::process::Output;
use tempfile::TempDir;

const KEY_FILE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../dbento-core/tests/fixtures/beekeeper/.key"
);
/// `"fake-pw-orders"` under the fixture key, IV [1; 16].
const SHOP_PW: &str = "546c5465a868fd187b201b92c667d6c7d4b86ddca7709e4abf7e6e348ae6b78d01010101010101010101010101010101jbe4qQIYNW68+1qffviXC2+1q7aazJxQ7S+5GETnkRs=";

/// A data folder: `Shop` with a saved password, `Nobody` without a user, and the fixture `.key`.
fn data_folder() -> TempDir {
    let dir = TempDir::new().unwrap();
    let db = rusqlite::Connection::open(dir.path().join("app.db")).unwrap();
    db.execute_batch(
        "CREATE TABLE saved_connection (id integer PRIMARY KEY, name varchar NOT NULL,
         connectionType varchar NOT NULL, host varchar, username varchar, defaultDatabase varchar, password varchar);",
    )
    .unwrap();
    db.execute(
        "INSERT INTO saved_connection VALUES
         (1, 'Shop', 'postgresql', 'db.shop.example', 'clerk', 'shop', ?1),
         (2, 'Nobody', 'postgresql', 'db.shop.example', '', 'stock', NULL)",
        [SHOP_PW],
    )
    .unwrap();
    std::fs::copy(KEY_FILE, dir.path().join(".key")).unwrap();
    dir
}

fn import(ws: &Path, source: &Path, extra: &[&str]) -> Output {
    dbento()
        .env("DBENTO_TEST_NO_KEYCHAIN", "1")
        .arg("--workspace")
        .arg(ws)
        .args(["import", "beekeeper"])
        .arg(source)
        .args(extra)
        .output()
        .unwrap()
}

fn json_of(out: &Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stdout)))
}

fn code(out: &Output) -> i32 {
    out.status.code().unwrap()
}

fn no_secrets() -> serde_json::Value {
    json!({"stored": [], "present": [], "failed": []})
}

#[test]
fn the_json_report_has_the_connection_the_skip_and_a_failed_secret_store() {
    let (src, ws) = (data_folder(), TempDir::new().unwrap());
    let out = import(ws.path(), src.path(), &["--format", "json"]);
    assert_eq!(code(&out), 0, "{}", String::from_utf8_lossy(&out.stderr));
    let doc = json_of(&out);
    assert_eq!(
        doc["connections"],
        json!([{"name": "shop", "from": "Shop", "action": "add"}])
    );
    assert_eq!(
        doc["skipped"],
        json!([{"kind": "data_source", "from": "Nobody", "reason": "no_user"}])
    );
    assert_eq!(
        doc["secrets"],
        json!({"stored": [], "present": [], "failed": ["shop"]})
    );
    assert_eq!(doc["next_steps"], json!(["dbento secret set shop"]));
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("warning: the secret for 'shop' was not stored"),
        "{stderr}"
    );
    let toml = std::fs::read_to_string(ws.path().join("dbento.toml")).unwrap();
    assert!(
        toml.contains("db.shop.example") && !toml.contains("fake-pw"),
        "{toml}"
    );
    assert!(!String::from_utf8_lossy(&out.stdout).contains("fake-pw"));
}

#[test]
fn the_app_db_file_is_accepted_and_a_dry_run_writes_nothing() {
    let (src, ws) = (data_folder(), TempDir::new().unwrap());
    let out = import(
        ws.path(),
        &src.path().join("app.db"),
        &["--format", "json", "--dry-run"],
    );
    assert_eq!(code(&out), 0, "{}", String::from_utf8_lossy(&out.stderr));
    let doc = json_of(&out);
    assert_eq!(doc["dry_run"], json!(true));
    assert_eq!(doc["connections"][0]["action"], "add");
    assert_eq!(doc["secrets"], no_secrets());
    assert!(!ws.path().join("dbento.toml").exists());
}

#[test]
fn no_secrets_never_reads_the_key_file() {
    let (src, ws) = (data_folder(), TempDir::new().unwrap());
    std::fs::write(src.path().join(".key"), "garbage").unwrap();
    let out = import(ws.path(), src.path(), &["--format", "json", "--no-secrets"]);
    assert_eq!(code(&out), 0, "{}", String::from_utf8_lossy(&out.stderr));
    let doc = json_of(&out);
    assert_eq!(doc["secrets"], no_secrets());
    assert_eq!(doc["warnings"], json!([]));
    assert_eq!(doc["next_steps"], json!(["dbento secret set shop"]));
}

#[test]
fn a_missing_key_file_warns_and_leaves_the_secret_to_the_user() {
    let (src, ws) = (data_folder(), TempDir::new().unwrap());
    std::fs::remove_file(src.path().join(".key")).unwrap();
    let out = import(ws.path(), src.path(), &["--format", "json"]);
    assert_eq!(code(&out), 0, "{}", String::from_utf8_lossy(&out.stderr));
    let doc = json_of(&out);
    let key = src.path().join(".key").display().to_string();
    assert_eq!(
        doc["warnings"],
        json!([{"from": key, "reason": "key_missing"}])
    );
    assert_eq!(doc["secrets"], no_secrets());
    assert_eq!(doc["next_steps"], json!(["dbento secret set shop"]));
}

#[test]
fn malformed_sources_exit_3_and_write_nothing() {
    let not_folder = TempDir::new().unwrap();
    let text_db = TempDir::new().unwrap();
    std::fs::write(
        text_db.path().join("app.db"),
        "plain text, not an SQLite database. ".repeat(20),
    )
    .unwrap();
    let no_table = TempDir::new().unwrap();
    rusqlite::Connection::open(no_table.path().join("app.db"))
        .unwrap()
        .execute_batch("CREATE TABLE favorite_query (id integer PRIMARY KEY)")
        .unwrap();
    let bad_key = data_folder();
    std::fs::write(bad_key.path().join(".key"), "garbage").unwrap();
    for (src, message) in [
        (not_folder.path(), "is not a Beekeeper Studio data folder"),
        (text_db.path(), "is not a Beekeeper Studio database"),
        (no_table.path(), "is not a Beekeeper Studio database"),
        (
            bad_key.path(),
            "cannot be decrypted as a Beekeeper Studio key file",
        ),
    ] {
        let ws = TempDir::new().unwrap();
        let out = import(ws.path(), src, &[]);
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert_eq!(code(&out), 3, "{stderr}");
        assert!(stderr.contains(message), "{stderr}");
        assert!(!ws.path().join("dbento.toml").exists());
    }
}
