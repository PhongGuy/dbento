//! SurrealDB through `dbento query` and `dbento run`: units, the rolled-back note, the level
//! and residual on stderr, the server warning and a saved `.surql` file.
use crate::common::{dbento, write_tree};
use dbento_testkit::{SrDb, surreal_db, surreal_server};
use serde_json::{Value, json};

const SECRET: &str = "DBENTO_TEST_SR_SECRET";

fn workspace(db: &SrDb, files: &[(&str, &str)]) -> tempfile::TempDir {
    let secret = format!("secret = {{ env = \"{SECRET}\" }}");
    let toml = format!(
        "dbento = 1\n{}",
        db.connection_toml("sr", Some(("root", "root")), &secret)
    );
    let mut all = vec![("dbento.toml", toml.as_str())];
    all.extend_from_slice(files);
    write_tree(&all)
}

/// Exit code, stdout and stderr of `dbento <args>` in `ws`.
fn dbento_in(db: &SrDb, ws: &tempfile::TempDir, args: &[&str]) -> (i32, String, String) {
    let out = dbento()
        .current_dir(ws.path())
        .env(SECRET, db.password())
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
async fn a_fenced_unknown_result_is_marked_rolled_back_with_the_level_and_residual() {
    let db = surreal_db().await;
    db.admin("DEFINE FUNCTION fn::one() { RETURN 1; };").await;
    let ws = workspace(&db, &[]);
    let args = [
        "query",
        "sr",
        "RETURN fn::one(); RETURN 2",
        "--read-only",
        "--format",
        "json",
    ];
    let (code, out, err) = dbento_in(&db, &ws, &args);
    assert_eq!(code, 0, "{err}");
    let s = statements(&out);
    assert_eq!(s[0]["rows"], json!([["1"]]), "{out}");
    assert_eq!(s[0]["rolled_back"], true, "{out}");
    assert!(
        s[1].get("rolled_back").is_none(),
        "a No result was not rolled back: {out}"
    );
    assert!(
        err.contains("best effort") && err.contains("ALTER SYSTEM"),
        "{err}"
    );
    let (_, _, err) = dbento_in(
        &db,
        &ws,
        &["query", "sr", "RETURN fn::one()", "--read-only"],
    );
    assert!(
        err.contains("(1 row, any changes were rolled back, "),
        "{err}"
    );
    // A LET has neither rows nor a value, so the table sink's no-columns line carries the note.
    let (code, _, err) = dbento_in(
        &db,
        &ws,
        &["query", "sr", "LET $x = fn::one()", "--read-only"],
    );
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("OK, any changes were rolled back ("), "{err}");
    let csv = [
        "query",
        "sr",
        "RETURN fn::one()",
        "--read-only",
        "--format",
        "csv",
    ];
    let (code, out, err) = dbento_in(&db, &ws, &csv);
    assert_eq!(code, 0, "{err}");
    assert!(out.contains('1'), "{out}");
    assert!(
        err.contains("statement 1: any changes were rolled back"),
        "{err}"
    );
}

#[tokio::test]
async fn a_saved_surql_file_runs_its_let_unit() {
    let db = surreal_db().await;
    let ws = workspace(
        &db,
        &[("queries/sr/answer.surql", "LET $a = 41;\nRETURN $a + 1;\n")],
    );
    let (code, out, err) = dbento_in(
        &db,
        &ws,
        &["run", "queries/sr/answer.surql", "--format", "json"],
    );
    assert_eq!(code, 0, "{err}");
    let s = statements(&out);
    assert_eq!(s.len(), 2, "{out}");
    assert_eq!(s[1]["rows"], json!([["42"]]), "{out}");
    assert!(
        !err.contains("note:"),
        "a writable session prints no level: {err}"
    );
}

#[tokio::test]
async fn a_newer_server_prints_its_warning() {
    let beta = surreal_server("v4.0.0-beta.1", &[]).await;
    let ws = workspace(&beta, &[]);
    let (code, _, err) = dbento_in(&beta, &ws, &["query", "sr", "RETURN 1"]);
    assert_eq!(code, 0, "{err}");
    assert!(err.contains("warning: SurrealDB 4.0.0-beta.1"), "{err}");
}
