//! SQL Server scripts through `dbento query`: every result of a batch, GO counts, the error
//! number, and stop wording.
use crate::common::{dbento, write_tree};
use dbento_testkit::{MS_SECRET_VAR, MsDb, mssql_db};
use serde_json::{Value, json};

fn workspace(db: &MsDb, extra: &str) -> tempfile::TempDir {
    let toml = format!(
        "dbento = 1\n{}",
        db.connection_toml("ms", &format!("tls = \"off\"\n{extra}"))
    );
    write_tree(&[("dbento.toml", toml.as_str())])
}

/// Exit code, stdout and stderr of `dbento query ms <sql>` with `args` after it.
fn query(db: &MsDb, ws: &tempfile::TempDir, sql: &str, args: &[&str]) -> (i32, String, String) {
    let out = dbento()
        .current_dir(ws.path())
        .env(MS_SECRET_VAR, db.password())
        .args(["query", "ms", sql])
        .args(args)
        .output()
        .unwrap();
    (
        out.status.code().unwrap(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn statements(stdout: &str) -> Vec<Value> {
    let doc: Value = serde_json::from_str(stdout).unwrap();
    doc["statements"].as_array().unwrap().clone()
}

#[tokio::test]
async fn every_result_of_a_batch_is_printed_and_go_repeats_it() {
    dbento_testkit::skip_without_mssql!();
    let db = mssql_db().await;
    let ws = workspace(&db, "");
    let sql = "SELECT 1 AS a SELECT 2 AS b\nGO 2\nSELECT 3 AS c";
    let (code, out, err) = query(&db, &ws, sql, &["--format", "json"]);
    assert_eq!(code, 0, "{err}");
    let rows: Vec<Value> = statements(&out).iter().map(|s| s["rows"].clone()).collect();
    assert_eq!(
        rows,
        [
            json!([["1"]]),
            json!([["2"]]),
            json!([["1"]]),
            json!([["2"]]),
            json!([["3"]])
        ]
    );
}

#[tokio::test]
async fn a_failed_statement_shows_its_error_number_with_the_results_before_it() {
    dbento_testkit::skip_without_mssql!();
    let db = mssql_db().await;
    let ws = workspace(&db, "");
    let (code, out, err) = query(&db, &ws, "SELECT 1 AS a SELECT 1/0 AS b", &[]);
    assert_ne!(code, 0);
    assert!(out.contains('1'), "{out}");
    assert!(
        err.contains("Divide by zero") && err.contains("(8134)"),
        "{err}"
    );
    let (code, out, _) = query(&db, &ws, "SELECT 1/0 AS b", &["--format", "json"]);
    assert_ne!(code, 0);
    let s = statements(&out);
    assert_eq!(s[0]["error"]["code"], "8134", "{out}");
    assert!(s[0]["error"].get("sqlstate").is_none(), "{out}");
}

#[tokio::test]
async fn a_timed_out_batch_says_what_it_may_have_kept() {
    dbento_testkit::skip_without_mssql!();
    let db = mssql_db().await;
    let ws = workspace(&db, "statement_timeout = \"500ms\"");
    let (code, _out, err) = query(&db, &ws, "WAITFOR DELAY '00:00:05'", &[]);
    assert_ne!(code, 0);
    assert!(err.contains("may have kept"), "{err}");
}
