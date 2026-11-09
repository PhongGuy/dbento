//! The DBeaver fixture through the binary: report snapshot, rerun, and running the imports.
use crate::common::{dbento, write_tree};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Output;
use tempfile::TempDir;

const MODERN: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../dbento-core/tests/fixtures/dbeaver/modern"
);

struct Setup {
    tmp: TempDir,
    project: PathBuf,
    ws: PathBuf,
}

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// Every file below `dir`, relative path to bytes.
fn files(dir: &Path) -> BTreeMap<PathBuf, Vec<u8>> {
    fn walk(base: &Path, dir: &Path, out: &mut BTreeMap<PathBuf, Vec<u8>>) {
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                walk(base, &path, out)
            } else {
                out.insert(
                    path.strip_prefix(base).unwrap().to_path_buf(),
                    std::fs::read(&path).unwrap(),
                );
            }
        }
    }
    let mut out = BTreeMap::new();
    walk(dir, dir, &mut out);
    out
}

fn setup() -> Setup {
    let tmp = TempDir::new().unwrap();
    let (project, ws) = (tmp.path().join("project"), tmp.path().join("ws"));
    copy_tree(Path::new(MODERN), &project);
    std::fs::create_dir_all(project.join("data")).unwrap();
    rusqlite::Connection::open(project.join("data/app.db"))
        .unwrap()
        .execute_batch("create table t(x); insert into t values (1), (2);")
        .unwrap();
    Setup { tmp, project, ws }
}

fn import(s: &Setup, extra: &[&str]) -> Output {
    dbento()
        .env("DBENTO_TEST_NO_KEYCHAIN", "1")
        .arg("--workspace")
        .arg(&s.ws)
        .args(["import", "dbeaver"])
        .arg(&s.project)
        .args(["--format", "json"])
        .args(extra)
        .output()
        .unwrap()
}

/// stdout with the tempdir as `<tmp>`, re-printed pretty with keys sorted.
fn pretty(s: &Setup, out: &Output) -> String {
    let text = String::from_utf8(out.stdout.clone())
        .unwrap()
        .replace(s.tmp.path().to_str().unwrap(), "<tmp>");
    let mut value: serde_json::Value = serde_json::from_str(&text).unwrap();
    value.sort_all_objects();
    serde_json::to_string_pretty(&value).unwrap()
}

#[test]
fn the_modern_report_then_a_rerun_that_changes_nothing() {
    let s = setup();
    let first = import(&s, &[]);
    assert_eq!(
        first.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&first.stderr)
    );
    insta::assert_snapshot!(pretty(&s, &first), @r#"
    {
      "connections": [
        {
          "action": "add",
          "from": "Orders DB",
          "name": "orders-db"
        },
        {
          "action": "add",
          "from": "Reporting (RO)",
          "name": "reporting-ro"
        },
        {
          "action": "add",
          "from": "app.db",
          "name": "app-db"
        },
        {
          "action": "add",
          "from": "Legacy Lite",
          "name": "legacy-lite"
        },
        {
          "action": "add",
          "from": "Orders DB",
          "name": "orders-db-2"
        }
      ],
      "dbento": 1,
      "dry_run": false,
      "next_steps": [
        "dbento secret set orders-db",
        "dbento secret set reporting-ro",
        "dbento secret set orders-db-2"
      ],
      "queries": [
        {
          "action": "create",
          "from": "<tmp>/project/Scripts/notes.sql",
          "path": "queries/notes.sql"
        },
        {
          "action": "create",
          "from": "<tmp>/project/Scripts/orders/top.sql",
          "path": "queries/orders-db/orders/top.sql"
        },
        {
          "action": "create",
          "from": "<tmp>/project/Scripts/tunnel.sql",
          "path": "queries/tunnel.sql"
        }
      ],
      "secrets": {
        "failed": [
          "orders-db",
          "reporting-ro",
          "orders-db-2"
        ],
        "present": [],
        "stored": []
      },
      "skipped": [
        {
          "from": "Tunnelled",
          "kind": "data_source",
          "reason": "ssh_tunnel"
        },
        {
          "from": "Proxied",
          "kind": "data_source",
          "reason": "proxy"
        },
        {
          "detail": "postgresql/postgres-timescale-jdbc",
          "from": "Timeseries",
          "kind": "data_source",
          "reason": "unsupported_driver"
        }
      ],
      "source": "<tmp>/project",
      "warnings": [
        {
          "detail": "loggerLevel",
          "from": "Orders DB",
          "reason": "dropped_parameter"
        },
        {
          "detail": "verify-ca",
          "from": "Orders DB",
          "reason": "tls_stricter"
        },
        {
          "from": "Orders DB",
          "reason": "ca_cert_missing"
        },
        {
          "from": "Reporting (RO)",
          "reason": "password_in_url"
        },
        {
          "detail": "prefer",
          "from": "Reporting (RO)",
          "reason": "tls_weaker"
        },
        {
          "from": "Reporting (RO)",
          "reason": "ca_cert_missing"
        },
        {
          "detail": "journal_mode",
          "from": "Legacy Lite",
          "reason": "dropped_parameter"
        },
        {
          "from": "Legacy Lite",
          "reason": "file_missing"
        },
        {
          "detail": "prefer",
          "from": "Orders DB",
          "reason": "tls_stricter"
        },
        {
          "from": "app.db",
          "reason": "machine_specific_path"
        },
        {
          "from": "Legacy Lite",
          "reason": "machine_specific_path"
        }
      ],
      "workspace": "<tmp>/ws"
    }
    "#);
    // The prod connection is tagged red; its dev namesake from the second storage is not.
    let toml = std::fs::read_to_string(s.ws.join("dbento.toml")).unwrap();
    let table = |name: &str| {
        let head = format!("connections.{name}]");
        toml.split('[')
            .find(|t| t.starts_with(&head))
            .unwrap_or_else(|| panic!("no {name} in {toml}"))
            .to_owned()
    };
    assert!(table("orders-db").contains("color = \"red\""), "{toml}");
    assert!(!table("orders-db-2").contains("color"), "{toml}");
    let stderr = String::from_utf8_lossy(&first.stderr);
    assert!(
        stderr.contains("warning: the secret for 'orders-db' was not stored"),
        "{stderr}"
    );
    let before = files(&s.ws);
    let second = import(&s, &[]);
    assert_eq!(second.status.code(), Some(0));
    let doc: serde_json::Value = serde_json::from_slice(&second.stdout).unwrap();
    for key in ["connections", "queries"] {
        for item in doc[key].as_array().unwrap() {
            assert_eq!(item["action"], "unchanged", "{item}");
        }
    }
    assert_eq!(doc["secrets"]["stored"], serde_json::json!([]));
    assert_eq!(files(&s.ws), before);
    let all = [
        String::from_utf8_lossy(&first.stdout).into_owned(),
        stderr.into_owned(),
    ];
    for text in before
        .values()
        .map(|b| String::from_utf8_lossy(b).into_owned())
        .chain(all)
    {
        assert!(
            !text.contains("fake-pw") && !text.contains("fake-ssh"),
            "{text}"
        );
    }
}

#[test]
fn the_imported_sqlite_connection_runs_a_query() {
    let s = setup();
    assert_eq!(import(&s, &[]).status.code(), Some(0));
    let out = dbento()
        .env("DBENTO_TEST_NO_KEYCHAIN", "1")
        .arg("--workspace")
        .arg(&s.ws)
        .args([
            "query",
            "app-db",
            "select count(*) from t",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(doc["statements"][0]["rows"], serde_json::json!([["2"]]));
}

#[tokio::test]
async fn the_imported_postgres_connection_runs_select_1() {
    let db = dbento_testkit::pg_db().await;
    let sources = serde_json::json!({"connections": {"postgres-jdbc-e2": {
        "provider": "postgresql", "driver": "postgres-jdbc", "name": "PG IT",
        "configuration": {"host": db.host(), "port": db.port().to_string(), "database": db.database(),
            "properties": {"sslmode": "disable"}}}}});
    let project = write_tree(&[(".dbeaver/data-sources.json", sources.to_string().as_str())]);
    let ws = TempDir::new().unwrap();
    let out = dbento()
        .env("DBENTO_TEST_NO_KEYCHAIN", "1")
        .arg("--workspace")
        .arg(ws.path())
        .args(["import", "dbeaver"])
        .arg(project.path())
        .args(["--default-user", db.user()])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = dbento()
        .arg("--workspace")
        .arg(ws.path())
        .args(["query", "pg-it", "select 1 as one", "--format", "json"])
        .env("DBENTO_TEST_NO_KEYCHAIN", "1")
        .env("DBENTO_SECRET_PG_IT", db.password())
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(doc["statements"][0]["rows"], serde_json::json!([["1"]]));
}
