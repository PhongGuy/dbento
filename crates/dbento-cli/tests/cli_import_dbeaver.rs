//! `dbento import dbeaver` on small synthetic projects; the keychain is always disabled.
mod common;
use common::{dbento, write_tree};
use serde_json::json;
use std::path::Path;
use std::process::Output;
use tempfile::TempDir;

const SHOP: &str = r#"{"folders":{"Prod":{}},"connections":{
"postgres-jdbc-1":{"provider":"postgresql","driver":"postgres-jdbc","name":"Shop","folder":"Prod","configuration":{"configurationType":"URL","url":"jdbc:postgresql://db.shop.example/shop?user=clerk&password=fake-pw-cli"}},
"postgres-jdbc-2":{"provider":"postgresql","driver":"postgres-jdbc","name":"Nobody","configuration":{"host":"db.shop.example","database":"stock"}}}}"#;

fn project() -> TempDir {
    write_tree(&[(".dbeaver/data-sources.json", SHOP)])
}

fn import(ws: &Path, project: &Path, extra: &[&str]) -> Output {
    dbento()
        .env("DBENTO_TEST_NO_KEYCHAIN", "1")
        .arg("--workspace")
        .arg(ws)
        .args(["import", "dbeaver"])
        .arg(project)
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
fn the_json_report_lists_actions_skips_warnings_secrets_and_next_steps() {
    let (p, ws) = (project(), TempDir::new().unwrap());
    let out = import(ws.path(), p.path(), &["--format", "json"]);
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
    let reasons: Vec<&str> = doc["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|w| w["reason"].as_str().unwrap())
        .collect();
    assert_eq!(reasons, ["password_in_url", "tls_stricter"]);
    assert_eq!(doc["secrets"], no_secrets());
    assert_eq!(doc["next_steps"], json!(["dbento secret set shop"]));
    let toml = std::fs::read_to_string(ws.path().join("dbento.toml")).unwrap();
    assert!(toml.contains("folder = \"Prod\""), "{toml}");
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    for text in [
        &toml,
        &String::from_utf8_lossy(&out.stdout).into_owned(),
        &stderr,
    ] {
        assert!(!text.contains("fake-pw"), "{text}");
    }
}

#[test]
fn a_dry_run_and_no_secrets_report_empty_secrets_and_every_added_connection() {
    for flag in ["--dry-run", "--no-secrets"] {
        let (p, ws) = (project(), TempDir::new().unwrap());
        let out = import(ws.path(), p.path(), &[flag, "--format", "json"]);
        assert_eq!(code(&out), 0, "{flag}");
        let doc = json_of(&out);
        assert_eq!(doc["secrets"], no_secrets(), "{flag}");
        assert_eq!(
            doc["next_steps"],
            json!(["dbento secret set shop"]),
            "{flag}"
        );
        assert_eq!(doc["dry_run"].as_bool(), Some(flag == "--dry-run"));
        let written = std::fs::read_dir(ws.path()).unwrap().count();
        assert_eq!(written == 0, flag == "--dry-run", "{flag}");
    }
}

#[test]
fn default_user_fills_a_missing_user_and_rename_takes_a_numbered_name() {
    let (p, ws) = (project(), TempDir::new().unwrap());
    let existing = "dbento = 1\n[connections.shop]\nengine = \"sqlite\"\npath = \"x.db\"\n";
    std::fs::write(ws.path().join("dbento.toml"), existing).unwrap();
    let args = [
        "--default-user",
        "fallback",
        "--on-conflict",
        "rename",
        "--format",
        "json",
    ];
    let out = import(ws.path(), p.path(), &args);
    assert_eq!(code(&out), 0, "{}", String::from_utf8_lossy(&out.stderr));
    let doc = json_of(&out);
    let names: Vec<(&str, &str)> = doc["connections"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| (c["name"].as_str().unwrap(), c["action"].as_str().unwrap()))
        .collect();
    assert_eq!(names, [("shop-2", "add"), ("nobody", "add")]);
    assert_eq!(
        doc["next_steps"],
        json!(["dbento secret set shop-2", "dbento secret set nobody"])
    );
}

#[test]
fn the_table_report_ends_with_the_secret_step() {
    let (p, ws) = (project(), TempDir::new().unwrap());
    let out = import(ws.path(), p.path(), &[]);
    assert_eq!(code(&out), 0);
    let text = String::from_utf8(out.stdout).unwrap();
    let lines: Vec<&str> = text.lines().collect();
    assert_eq!(
        lines[lines.len() - 2..],
        ["next steps:", "dbento secret set shop"],
        "{text}"
    );
}

#[test]
fn unreadable_projects_exit_3_and_write_nothing() {
    let ws = TempDir::new().unwrap();
    let empty = TempDir::new().unwrap();
    let root = write_tree(&[
        ("General/.dbeaver/data-sources.json", "{}"),
        ("Work/.dbeaver/data-sources.json", "{}"),
    ]);
    let encrypted = write_tree(&[
        (".dbeaver/data-sources.json", "{}"),
        (
            ".dbeaver/project-settings.json",
            r#"{"secureProject":true}"#,
        ),
    ]);
    let malformed = write_tree(&[(".dbeaver/data-sources.json", r#"{"connections": {"#)]);
    let bad_creds = write_tree(&[
        (".dbeaver/data-sources.json", SHOP),
        (".dbeaver/credentials-config.json", "not encrypted at all"),
    ]);
    for (dir, needle) in [
        (empty.path(), "not a DBeaver project"),
        (root.path(), "General, Work"),
        (encrypted.path(), "encrypted DBeaver project"),
        (malformed.path(), "data-sources.json:1: invalid JSON"),
        (bad_creds.path(), "cannot be decrypted"),
    ] {
        let out = import(ws.path(), dir, &[]);
        let err = String::from_utf8_lossy(&out.stderr);
        assert_eq!(code(&out), 3, "{needle}: {err}");
        assert!(err.contains(needle), "{needle}: {err}");
        assert_eq!(std::fs::read_dir(ws.path()).unwrap().count(), 0, "{needle}");
    }
}

#[cfg(unix)]
#[test]
fn a_failed_apply_stores_no_secret() {
    use std::os::unix::fs::PermissionsExt;
    let creds = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../dbento-core/tests/fixtures/dbeaver/modern/.dbeaver/credentials-config.json"
    );
    let sources = r#"{"connections":{"postgres-jdbc-a1":{"provider":"postgresql","driver":"postgres-jdbc","name":"Orders DB","save-password":true,"configuration":{"host":"db.orders.example","database":"orders"}}}}"#;
    let metadata = r#"{"resources":{"Scripts/top.sql":{"default-datasource":"postgres-jdbc-a1"}}}"#;
    let p = write_tree(&[
        (".dbeaver/data-sources.json", sources),
        (".dbeaver/project-metadata.json", metadata),
        ("Scripts/top.sql", "select 1;\n"),
    ]);
    std::fs::copy(creds, p.path().join(".dbeaver/credentials-config.json")).unwrap();
    let ws = TempDir::new().unwrap();
    let queries = ws.path().join("queries");
    std::fs::create_dir(&queries).unwrap();
    std::fs::set_permissions(&queries, std::fs::Permissions::from_mode(0o555)).unwrap();
    // Root ignores the mode bits, so the failure this test needs cannot happen there.
    if std::fs::write(queries.join("probe"), "x").is_ok() {
        std::fs::remove_file(queries.join("probe")).unwrap();
        std::fs::set_permissions(&queries, std::fs::Permissions::from_mode(0o755)).unwrap();
        eprintln!("skipped: the read-only folder is writable (running as root)");
        return;
    }
    let out = import(ws.path(), p.path(), &["--format", "json"]);
    std::fs::set_permissions(&queries, std::fs::Permissions::from_mode(0o755)).unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(matches!(code(&out), 1 | 3), "{stderr}");
    assert!(!stderr.contains("was not stored"), "{stderr}");
    assert!(!ws.path().join("dbento.toml").exists());
    assert_eq!(json_of(&out)["secrets"], no_secrets());
}

#[test]
fn the_datagrip_report_has_no_secrets_key() {
    let xml = r#"<project version="4"><component name="DataSourceManagerImpl"><data-source source="LOCAL" name="Lite" uuid="aaaaaaaa-0000-4000-8000-000000000009"><driver-ref>sqlite.xerial</driver-ref><jdbc-url>jdbc:sqlite:x.db</jdbc-url></data-source></component></project>"#;
    let (p, ws) = (
        write_tree(&[(".idea/dataSources.xml", xml)]),
        TempDir::new().unwrap(),
    );
    let out = dbento()
        .arg("--workspace")
        .arg(ws.path())
        .args(["import", "datagrip"])
        .arg(p.path())
        .args(["--no-consoles", "--format", "json"])
        .output()
        .unwrap();
    assert_eq!(code(&out), 0, "{}", String::from_utf8_lossy(&out.stderr));
    assert!(json_of(&out).get("secrets").is_none());
}
