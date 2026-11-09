//! The Beekeeper fixture through the binary: report snapshot, rerun, and running the imports.
use crate::common::dbento;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Output;
use tempfile::TempDir;

const FIXTURES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../dbento-core/tests/fixtures/beekeeper"
);
const FIXTURE_KEY: &str = "0f1e2d3c4b5a69788796a5b4c3d2e1f00f1e2d3c4b5a69788796a5b4c3d2e1f0";

struct Setup {
    tmp: TempDir,
    source: PathBuf,
    ws: PathBuf,
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

/// `<tmp>/beekeeper-studio` from modern.sql with the fixture `.key` and `lite/shop.db` (2 rows).
fn setup() -> Setup {
    let tmp = TempDir::new().unwrap();
    let (source, ws) = (tmp.path().join("beekeeper-studio"), tmp.path().join("ws"));
    std::fs::create_dir_all(source.join("lite")).unwrap();
    let script = std::fs::read_to_string(Path::new(FIXTURES).join("modern.sql")).unwrap();
    rusqlite::Connection::open(source.join("app.db"))
        .unwrap()
        .execute_batch(&script)
        .unwrap();
    std::fs::copy(Path::new(FIXTURES).join(".key"), source.join(".key")).unwrap();
    rusqlite::Connection::open(source.join("lite/shop.db"))
        .unwrap()
        .execute_batch("create table t(x); insert into t values (1), (2);")
        .unwrap();
    Setup { tmp, source, ws }
}

fn import(s: &Setup) -> Output {
    import_as(s, "json")
}

fn import_as(s: &Setup, format: &str) -> Output {
    dbento()
        .env("DBENTO_TEST_NO_KEYCHAIN", "1")
        .arg("--workspace")
        .arg(&s.ws)
        .args(["import", "beekeeper"])
        .arg(&s.source)
        .args(["--format", format])
        .output()
        .unwrap()
}

/// Fails when `text` holds a fixture password or the fixture key.
fn assert_no_secret(text: &str) {
    assert!(
        !text.contains("fake-pw") && !text.contains("fake-ssh") && !text.contains(FIXTURE_KEY),
        "{text}"
    );
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
    let source_before = files(&s.source);
    let first = import(&s);
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
          "from": "Local socket",
          "name": "local-socket"
        },
        {
          "action": "add",
          "from": "Client certs",
          "name": "client-certs"
        },
        {
          "action": "add",
          "from": "IAM",
          "name": "iam"
        },
        {
          "action": "add",
          "from": "Shop lite",
          "name": "shop-lite"
        },
        {
          "action": "add",
          "from": "Abs lite",
          "name": "abs-lite"
        },
        {
          "action": "add",
          "from": "Broken secret",
          "name": "broken-secret"
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
        "dbento secret set local-socket",
        "dbento secret set client-certs",
        "dbento secret set iam",
        "dbento secret set broken-secret",
        "dbento secret set orders-db-2"
      ],
      "queries": [
        {
          "action": "create",
          "from": "<tmp>/beekeeper-studio/app.db#favorite_query/1",
          "path": "queries/Reports/Top orders.sql"
        },
        {
          "action": "unchanged",
          "from": "<tmp>/beekeeper-studio/app.db#favorite_query/2",
          "path": "queries/Reports/Top orders.sql"
        },
        {
          "action": "create",
          "from": "<tmp>/beekeeper-studio/app.db#favorite_query/3",
          "path": "queries/Reports/Top orders-2.sql"
        },
        {
          "action": "create",
          "from": "<tmp>/beekeeper-studio/app.db#favorite_query/4",
          "path": "queries/Reports/Monthly _ Q_/a_b_ c_.sql"
        },
        {
          "action": "create",
          "from": "<tmp>/beekeeper-studio/app.db#favorite_query/6",
          "path": "queries/_CON.sql"
        },
        {
          "action": "create",
          "from": "<tmp>/beekeeper-studio/app.db#favorite_query/7",
          "path": "queries/query.sql"
        }
      ],
      "secrets": {
        "failed": [
          "orders-db",
          "reporting-ro",
          "client-certs",
          "orders-db-2"
        ],
        "present": [],
        "stored": []
      },
      "skipped": [
        {
          "detail": "cockroachdb",
          "from": "Cockroach",
          "kind": "data_source",
          "reason": "unsupported_driver"
        },
        {
          "from": "Tunnelled",
          "kind": "data_source",
          "reason": "ssh_tunnel"
        },
        {
          "from": "Cloud",
          "kind": "data_source",
          "reason": "cloud_workspace"
        },
        {
          "from": "No user",
          "kind": "data_source",
          "reason": "no_user"
        },
        {
          "from": "<tmp>/beekeeper-studio/app.db#favorite_query/5",
          "kind": "query",
          "reason": "empty_query"
        }
      ],
      "source": "<tmp>/beekeeper-studio",
      "warnings": [
        {
          "from": "Orders DB",
          "reason": "ca_cert_missing"
        },
        {
          "detail": "require",
          "from": "Reporting (RO)",
          "reason": "tls_stricter"
        },
        {
          "detail": "require",
          "from": "Client certs",
          "reason": "tls_stricter"
        },
        {
          "from": "Client certs",
          "reason": "client_cert_missing"
        },
        {
          "detail": "aws_iam",
          "from": "IAM",
          "reason": "auth_model"
        },
        {
          "from": "Abs lite",
          "reason": "file_missing"
        },
        {
          "from": "Broken secret",
          "reason": "secret_unreadable"
        },
        {
          "from": "Shop lite",
          "reason": "machine_specific_path"
        },
        {
          "from": "Abs lite",
          "reason": "machine_specific_path"
        }
      ],
      "workspace": "<tmp>/ws"
    }
    "#);
    // The red-labelled connection is tagged red; its unlabelled namesake is not.
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
    let stderr = String::from_utf8_lossy(&first.stderr).into_owned();
    assert!(
        stderr.contains("warning: the secret for 'orders-db' was not stored"),
        "{stderr}"
    );
    let before = files(&s.ws);
    let second = import(&s);
    assert_eq!(second.status.code(), Some(0));
    let doc: serde_json::Value = serde_json::from_slice(&second.stdout).unwrap();
    for key in ["connections", "queries"] {
        for item in doc[key].as_array().unwrap() {
            assert_eq!(item["action"], "unchanged", "{item}");
        }
    }
    assert_eq!(doc["secrets"]["stored"], serde_json::json!([]));
    assert_eq!(files(&s.ws), before);
    assert_eq!(files(&s.source), source_before);
    let outputs = [&first.stdout, &second.stdout, &second.stderr]
        .map(|b| String::from_utf8_lossy(b).into_owned());
    for text in before
        .values()
        .map(|b| String::from_utf8_lossy(b).into_owned())
        .chain(outputs)
    {
        assert_no_secret(&text);
    }
    assert_no_secret(&stderr);
}

#[test]
fn the_table_report_names_no_password() {
    let s = setup();
    let out = import_as(&s, "table");
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("orders-db"), "{stdout}");
    assert_no_secret(&stdout);
    assert_no_secret(&String::from_utf8_lossy(&out.stderr));
    for bytes in files(&s.ws).values() {
        assert_no_secret(&String::from_utf8_lossy(bytes));
    }
}

#[test]
fn the_imported_sqlite_connection_runs_a_query() {
    let s = setup();
    assert_eq!(import(&s).status.code(), Some(0));
    let out = dbento()
        .env("DBENTO_TEST_NO_KEYCHAIN", "1")
        .arg("--workspace")
        .arg(&s.ws)
        .args([
            "query",
            "shop-lite",
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
    let src = TempDir::new().unwrap();
    let app = rusqlite::Connection::open(src.path().join("app.db")).unwrap();
    app.execute_batch(
        "CREATE TABLE saved_connection (id integer PRIMARY KEY, name varchar NOT NULL, connectionType varchar NOT NULL,
         host varchar, port integer, username varchar, defaultDatabase varchar, ssl boolean NOT NULL DEFAULT 0);",
    )
    .unwrap();
    app.execute(
        "INSERT INTO saved_connection (id, name, connectionType, host, port, username, defaultDatabase)
         VALUES (1, 'PG IT', 'postgresql', ?1, ?2, ?3, ?4)",
        rusqlite::params![db.host(), db.port(), db.user(), db.database()],
    )
    .unwrap();
    drop(app);
    let ws = TempDir::new().unwrap();
    let out = dbento()
        .env("DBENTO_TEST_NO_KEYCHAIN", "1")
        .arg("--workspace")
        .arg(ws.path())
        .args(["import", "beekeeper"])
        .arg(src.path())
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
