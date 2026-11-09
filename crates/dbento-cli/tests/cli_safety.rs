use crate::common::{LONG, count, dbento, query_json, redact, sqlite_ws};
use std::time::{Duration, Instant};

const T: &str =
    "CREATE TABLE t(id INTEGER, note TEXT); INSERT INTO t VALUES (1, 'hello'), (2, NULL)";

#[test]
fn the_spec_example_refuses_an_unguarded_delete_without_a_tty() {
    let ws = sqlite_ws(T);
    let out = dbento()
        .current_dir(ws.path())
        .args([
            "query",
            "lite",
            "select id, note from t; delete from t",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    insta::assert_snapshot!(redact(&out.stdout), @r#"
{"dbento":1,"connection":"lite","statements":[
{"index":0,"sql":"select id, note from t;","columns":[{"name":"id","type":"INTEGER"},{"name":"note","type":"TEXT"}],"rows":[
["1","hello"],
["2",null]
],"row_count":2,"truncated":false,"rows_affected":null,"duration_ms":0,"status":"ok"},
{"index":1,"sql":"delete from t","status":"refused","reason":"delete_without_where"}
]}
"#);
    assert_eq!(count(&ws, "select count(*) from t"), 2);
}

#[test]
fn a_refusal_stops_the_script() {
    let ws = sqlite_ws(T);
    let (code, doc) = query_json(
        ws.path(),
        "update t set note = 'x'; insert into t values (3, 'late')",
        &[],
    );
    assert_eq!(code, 2);
    assert_eq!(doc["statements"].as_array().unwrap().len(), 1);
    assert_eq!(doc["statements"][0]["reason"], "update_without_where");
    assert_eq!(count(&ws, "select count(*) from t"), 2);
}

#[test]
fn yes_runs_it() {
    let ws = sqlite_ws(T);
    let (code, doc) = query_json(ws.path(), "delete from t", &["--yes"]);
    assert_eq!(code, 0);
    assert_eq!(doc["statements"][0]["rows_affected"], 2);
}

#[test]
fn a_terminal_answer_decides() {
    for (answer, code, left) in [("y\n", 0, 0), ("YES\n", 0, 0), ("n\n", 2, 2), ("", 2, 2)] {
        let ws = sqlite_ws(T);
        let out = dbento()
            .current_dir(ws.path())
            .env("DBENTO_TEST_ASSUME_TTY", "1")
            .args(["query", "lite", "delete from t", "--format", "json"])
            .write_stdin(answer)
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(code), "{answer:?}");
        assert!(
            String::from_utf8_lossy(&out.stderr)
                .contains("DELETE without WHERE on 'lite'; run it? [y/N]")
        );
        assert_eq!(count(&ws, "select count(*) from t"), left, "{answer:?}");
    }
}

#[test]
fn a_read_only_session_lets_the_engine_refuse_without_asking() {
    let ws = sqlite_ws(T);
    let out = dbento()
        .current_dir(ws.path())
        .env("DBENTO_TEST_ASSUME_TTY", "1")
        .args([
            "query",
            "lite",
            "delete from t",
            "--format",
            "json",
            "--read-only",
        ])
        .write_stdin("y\n")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(!String::from_utf8_lossy(&out.stderr).contains("[y/N]"));
    let doc: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(doc["statements"][0]["error"]["code"], "read_only");
    assert_eq!(count(&ws, "select count(*) from t"), 2);
}

#[test]
fn attach_on_a_read_only_session_is_a_read_only_violation() {
    let ws = sqlite_ws(T);
    let (code, doc) = query_json(ws.path(), "attach 'other.db' as o", &["--read-only"]);
    assert_eq!(code, 1);
    assert_eq!(doc["statements"][0]["error"]["code"], "read_only");
}

#[test]
fn timeout_stops_a_long_statement() {
    let ws = sqlite_ws(T);
    let started = Instant::now();
    let (code, doc) = query_json(ws.path(), LONG, &["--timeout", "100ms"]);
    assert!(started.elapsed() < Duration::from_secs(5));
    assert_eq!(code, 1);
    assert_eq!(doc["statements"][0]["error"]["code"], "timeout");
    let out = dbento()
        .current_dir(ws.path())
        .args(["query", "lite", "select 1", "--timeout", "soon"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
}

#[test]
fn drop_asks_in_its_own_words_and_runs_with_yes() {
    let ws = sqlite_ws(T);
    let tables = "select count(*) from sqlite_master where name = 't'";
    let (code, doc) = query_json(ws.path(), "drop table t", &[]);
    assert_eq!(code, 2);
    assert_eq!(doc["statements"][0]["reason"], "drop");
    let out = dbento()
        .current_dir(ws.path())
        .env("DBENTO_TEST_ASSUME_TTY", "1")
        .args(["query", "lite", "drop table t", "--format", "json"])
        .write_stdin("n\n")
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    assert!(
        String::from_utf8_lossy(&out.stderr).contains("DROP on 'lite'; run it? [y/N]"),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let out = dbento()
        .current_dir(ws.path())
        .args(["query", "lite", "drop table t"])
        .output()
        .unwrap();
    assert!(
        String::from_utf8_lossy(&out.stderr)
            .contains("refused: statement 1: DROP; pass --yes to run it")
    );
    assert_eq!(count(&ws, tables), 1);
    let (code, _) = query_json(ws.path(), "drop table t", &["--yes"]);
    assert_eq!(code, 0);
    assert_eq!(count(&ws, tables), 0);
}

#[test]
fn every_sqlite_drop_kind_is_refused_without_yes() {
    let ws = sqlite_ws(
        "CREATE TABLE t(a); CREATE INDEX i ON t(a); CREATE VIEW v AS SELECT a FROM t; \
         CREATE TRIGGER tr AFTER INSERT ON t BEGIN SELECT 1; END;",
    );
    for sql in [
        "drop index i",
        "drop view v",
        "drop trigger tr",
        "drop table if exists t",
    ] {
        let (code, doc) = query_json(ws.path(), sql, &[]);
        assert_eq!(code, 2, "{sql}");
        assert_eq!(doc["statements"][0]["reason"], "drop", "{sql}");
    }
    assert_eq!(count(&ws, "select count(*) from sqlite_master"), 4);
}
