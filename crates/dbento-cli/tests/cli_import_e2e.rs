//! The modern fixture through the binary: report snapshot, rerun, dry run, and running the imports.
mod common;
use common::{dbento, write_tree};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Output;
use tempfile::TempDir;

const FIXTURES: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../dbento-core/tests/fixtures/datagrip"
);

struct Setup {
    tmp: TempDir,
    project: PathBuf,
    config: PathBuf,
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
    let (project, config, ws) = (
        tmp.path().join("project"),
        tmp.path().join("jetbrains"),
        tmp.path().join("ws"),
    );
    copy_tree(&Path::new(FIXTURES).join("modern"), &project);
    copy_tree(&Path::new(FIXTURES).join("jetbrains"), &config);
    std::fs::create_dir_all(project.join("data")).unwrap();
    rusqlite::Connection::open(project.join("data/app.db"))
        .unwrap()
        .execute_batch("create table t(x); insert into t values (1), (2);")
        .unwrap();
    Setup {
        tmp,
        project,
        config,
        ws,
    }
}

fn import(s: &Setup, extra: &[&str]) -> Output {
    dbento()
        .arg("--workspace")
        .arg(&s.ws)
        .args(["import", "datagrip"])
        .arg(&s.project)
        .arg("--jetbrains-config")
        .arg(&s.config)
        .args(["--format", "json"])
        .args(extra)
        .output()
        .unwrap()
}

/// stdout with the tempdir as `<tmp>`, re-printed pretty with keys sorted. The sort is
/// explicit because a workspace build can unify serde_json's `preserve_order` feature in.
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
        }
      ],
      "dbento": 1,
      "dry_run": false,
      "next_steps": [
        "dbento secret set orders-db",
        "dbento secret set reporting-ro"
      ],
      "queries": [
        {
          "action": "create",
          "from": "<tmp>/project/queries/notes.sql",
          "path": "queries/notes.sql"
        },
        {
          "action": "create",
          "from": "<tmp>/project/queries/orders/top.sql",
          "path": "queries/orders-db/orders/top.sql"
        },
        {
          "action": "create",
          "from": "<tmp>/project/.idea/queries/Query.sql",
          "path": "queries/Query.sql"
        },
        {
          "action": "create",
          "from": "<tmp>/jetbrains/DataGrip2024.2/consoles/db/11111111-1111-4111-8111-111111111111/console.sql",
          "path": "queries/orders-db/console.sql"
        },
        {
          "action": "create",
          "from": "<tmp>/jetbrains/PyCharm2024.1/consoles/db/11111111-1111-4111-8111-111111111111/console.sql",
          "path": "queries/orders-db/console-PyCharm2024.1.sql"
        }
      ],
      "skipped": [
        {
          "detail": "redis",
          "from": "Cache",
          "kind": "data_source",
          "reason": "unsupported_driver"
        },
        {
          "from": "Tunnelled",
          "kind": "data_source",
          "reason": "ssh_tunnel"
        },
        {
          "from": "<tmp>/jetbrains/DataGrip2024.2/consoles/db/55555555-5555-4555-8555-555555555555/console.sql",
          "kind": "query",
          "reason": "source_skipped"
        }
      ],
      "source": "<tmp>/project",
      "warnings": [
        {
          "from": "Orders DB",
          "reason": "password_in_url"
        },
        {
          "detail": "loginTimeout",
          "from": "Orders DB",
          "reason": "dropped_parameter"
        },
        {
          "detail": "require",
          "from": "Orders DB",
          "reason": "tls_stricter"
        },
        {
          "from": "Orders DB",
          "reason": "ca_cert_missing"
        },
        {
          "detail": "prefer",
          "from": "Reporting (RO)",
          "reason": "tls_weaker"
        },
        {
          "from": "app.db",
          "reason": "machine_specific_path"
        }
      ],
      "workspace": "<tmp>/ws"
    }
    "#);
    let before = files(&s.ws);
    let second = import(&s, &[]);
    assert_eq!(second.status.code(), Some(0));
    let doc: serde_json::Value = serde_json::from_slice(&second.stdout).unwrap();
    for key in ["connections", "queries"] {
        for item in doc[key].as_array().unwrap() {
            assert_eq!(item["action"], "unchanged", "{item}");
        }
    }
    assert_eq!(doc["next_steps"], serde_json::json!([]));
    assert_eq!(files(&s.ws), before);
    assert!(
        !before
            .values()
            .any(|b| String::from_utf8_lossy(b).contains("hunter2"))
    );
    assert!(!String::from_utf8_lossy(&first.stdout).contains("hunter2"));
}

#[test]
fn a_dry_run_leaves_the_workspace_empty() {
    let s = setup();
    std::fs::create_dir(&s.ws).unwrap();
    let out = import(&s, &["--dry-run"]);
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&out.stdout).unwrap()["dry_run"],
        true
    );
    assert_eq!(std::fs::read_dir(&s.ws).unwrap().count(), 0);
}

#[test]
fn the_imported_sqlite_connection_runs_a_query() {
    let s = setup();
    assert_eq!(import(&s, &[]).status.code(), Some(0));
    let out = dbento()
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
    let host = if db.host().contains(':') {
        format!("[{}]", db.host())
    } else {
        db.host().to_owned()
    };
    let uuid = "aaaaaaaa-0000-4000-8000-0000000000e2";
    let sources = format!(
        r#"<project version="4"><component name="DataSourceManagerImpl"><data-source source="LOCAL" name="PG IT" uuid="{uuid}"><driver-ref>postgresql</driver-ref><jdbc-url>jdbc:postgresql://{host}:{}/{}?sslmode=disable</jdbc-url></data-source></component></project>"#,
        db.port(),
        db.database()
    );
    let local = format!(
        r#"<project version="4"><component name="dataSourceStorageLocal"><data-source name="PG IT" uuid="{uuid}"><user-name>{}</user-name></data-source></component></project>"#,
        db.user()
    );
    let project = write_tree(&[
        (".idea/dataSources.xml", sources.as_str()),
        (".idea/dataSources.local.xml", local.as_str()),
    ]);
    let ws = TempDir::new().unwrap();
    let out = dbento()
        .arg("--workspace")
        .arg(ws.path())
        .args(["import", "datagrip"])
        .arg(project.path())
        .arg("--no-consoles")
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
