mod common;
use common::{LITE_TOML, count, dbento, query_json, redact, sqlite_ws, write_tree};

const T: &str =
    "CREATE TABLE t(id INTEGER, note TEXT); INSERT INTO t VALUES (1, 'hello'), (2, NULL)";

#[test]
fn the_json_document_streams_in_spec_key_order() {
    let ws = sqlite_ws(T);
    let out = dbento()
        .current_dir(ws.path())
        .args([
            "query",
            "lite",
            "select id, note from t; insert into t values (3, 'x'); select * from nope",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    insta::assert_snapshot!(redact(&out.stdout), @r#"
{"dbento":1,"connection":"lite","statements":[
{"index":0,"sql":"select id, note from t;","columns":[{"name":"id","type":"INTEGER"},{"name":"note","type":"TEXT"}],"rows":[
["1","hello"],
["2",null]
],"row_count":2,"truncated":false,"rows_affected":null,"duration_ms":0,"status":"ok"},
{"index":1,"sql":"insert into t values (3, 'x');","columns":[],"rows":[],"row_count":0,"truncated":false,"rows_affected":1,"duration_ms":0,"status":"ok"},
{"index":2,"sql":"select * from nope","status":"error","error":{"code":"1","message":"no such table: nope"}}
]}
"#);
}

#[test]
fn an_empty_script_is_an_empty_document() {
    let ws = sqlite_ws(T);
    let out = dbento()
        .current_dir(ws.path())
        .args(["query", "lite", "-- nothing", "--format", "json"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "{\"dbento\":1,\"connection\":\"lite\",\"statements\":[\n]}\n"
    );
}

#[test]
fn max_rows_truncates_and_bytes_are_hex() {
    let ws = sqlite_ws("CREATE TABLE b(v); INSERT INTO b VALUES (x'00ff'), (x'')");
    let (code, doc) = query_json(ws.path(), "select v from b", &["--max-rows", "1"]);
    assert_eq!(code, 0);
    let s = &doc["statements"][0];
    assert_eq!(s["rows"], serde_json::json!([["\\x00ff"]]));
    assert_eq!(
        (s["row_count"].as_u64(), s["truncated"].as_bool()),
        (Some(1), Some(true))
    );
}

#[test]
fn a_failure_after_partial_rows_reports_status_error() {
    let ws = sqlite_ws("");
    // Row 2000 overflows, after the first full batch of 1,024 rows has been streamed.
    let sql = "WITH RECURSIVE r(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM r WHERE x < 2000) \
               SELECT CASE WHEN x = 2000 THEN abs(-9223372036854775807 - 1) ELSE x END AS v FROM r; select 1";
    let (code, doc) = query_json(ws.path(), sql, &[]);
    assert_eq!(code, 1);
    let statements = doc["statements"].as_array().unwrap();
    assert_eq!(statements.len(), 1, "execution stops at the first error");
    let s = &statements[0];
    let rows = s["rows"].as_array().unwrap();
    assert!(!rows.is_empty(), "partial rows were streamed");
    assert_eq!(s["row_count"].as_u64(), Some(rows.len() as u64));
    assert_eq!(s["status"], "error");
    assert_eq!(s["error"]["code"], "1");
    assert_eq!(s["error"]["message"], "integer overflow");
}

#[test]
fn an_unknown_connection_or_missing_file_is_exit_3_with_empty_stdout() {
    let ws = write_tree(&[("dbento.toml", LITE_TOML)]);
    for conn in ["nope", "lite"] {
        let out = dbento()
            .current_dir(ws.path())
            .args(["query", conn, "select 1", "--format", "json"])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(3), "{conn}");
        assert!(out.stdout.is_empty(), "{conn}");
        assert!(
            String::from_utf8_lossy(&out.stderr).starts_with("error: "),
            "{conn}"
        );
    }
}

#[test]
fn a_truncated_write_whose_commit_fails_reports_the_error_in_json() {
    let ws = sqlite_ws(
        "CREATE TABLE p(a INTEGER PRIMARY KEY);
         CREATE TABLE t(a INTEGER REFERENCES p DEFERRABLE INITIALLY DEFERRED)",
    );
    // No parent row exists, so the deferred key check fails the write's commit.
    let sql = "PRAGMA foreign_keys = ON; INSERT INTO t WITH RECURSIVE r(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM r WHERE x < 10) SELECT x FROM r RETURNING a";
    let (code, doc) = query_json(ws.path(), sql, &["--max-rows", "1"]);
    assert_eq!(code, 1);
    // SQLite resets a truncated write inline, so the statement itself reports the failed commit.
    let failed = &doc["statements"][1];
    assert_eq!(failed["status"], "error", "{doc}");
    assert_eq!(failed["error"]["code"], "787", "{doc}");
    assert_eq!(count(&ws, "SELECT count(*) FROM t"), 0);
}
