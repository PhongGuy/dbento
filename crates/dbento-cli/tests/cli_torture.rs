// crates/dbento-cli/tests/cli_torture.rs
mod common;
use common::{count, dbento, query_json, redact, sqlite_ws};

const FIXTURE: &str = include_str!("fixtures/sqlite_torture.sql");
const TABLES: [&str; 15] = [
    "affinity_mix",
    "declared",
    "strict_t",
    "no_rowid",
    "alias_pk",
    "int_pk",
    "edges",
    "generated",
    "v_declared",
    "audited",
    "audit_log",
    "sqlite_sequence",
    "docs",
    "docs_content",
    "docs_config",
];

#[test]
fn every_torture_table_round_trips_as_text() {
    let ws = sqlite_ws(FIXTURE);
    let sql: String = TABLES
        .iter()
        .map(|t| format!("select * from {t};\n"))
        .collect();
    let out = dbento()
        .current_dir(ws.path())
        .args(["query", "lite", &sql, "--format", "json"])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    insta::assert_snapshot!(redact(&out.stdout), @r#"
{"dbento":1,"connection":"lite","statements":[
{"index":0,"sql":"select * from affinity_mix;","columns":[{"name":"id","type":"INTEGER"},{"name":"v","type":""}],"rows":[
["1","42"],
["2","2.5"],
["3","text"],
["4","\\x00ff"],
["5",null]
],"row_count":5,"truncated":false,"rows_affected":null,"duration_ms":0,"status":"ok"},
{"index":1,"sql":"select * from declared;","columns":[{"name":"a","type":"VARCHAR(10)"},{"name":"b","type":"BOOLEAN"},{"name":"c","type":"DATETIME"},{"name":"d","type":"FLOATING POINT"},{"name":"e","type":"DECIMAL(10,2)"},{"name":"f","type":""}],"rows":[
["longer than ten","true","2024-01-02 03:04:05","2","1.1","007"],
["1","1","1700000000","1.5","3","7"]
],"row_count":2,"truncated":false,"rows_affected":null,"duration_ms":0,"status":"ok"},
{"index":2,"sql":"select * from strict_t;","columns":[{"name":"i","type":"INTEGER"},{"name":"r","type":"REAL"},{"name":"t","type":"TEXT"},{"name":"b","type":"BLOB"},{"name":"a","type":"ANY"}],"rows":[
["1","2.0","x","\\x01","1"],
[null,"0.5","","\\x","1.0"]
],"row_count":2,"truncated":false,"rows_affected":null,"duration_ms":0,"status":"ok"},
{"index":3,"sql":"select * from no_rowid;","columns":[{"name":"k","type":"TEXT"},{"name":"v","type":"INT"}],"rows":[
["a","1"],
["b","2"]
],"row_count":2,"truncated":false,"rows_affected":null,"duration_ms":0,"status":"ok"},
{"index":4,"sql":"select * from alias_pk;","columns":[{"name":"id","type":"INTEGER"},{"name":"v","type":"TEXT"}],"rows":[
["1","auto"],
["10","ten"]
],"row_count":2,"truncated":false,"rows_affected":null,"duration_ms":0,"status":"ok"},
{"index":5,"sql":"select * from int_pk;","columns":[{"name":"id","type":"INT"},{"name":"v","type":"TEXT"}],"rows":[
[null,"null key"],
["10","ten"]
],"row_count":2,"truncated":false,"rows_affected":null,"duration_ms":0,"status":"ok"},
{"index":6,"sql":"select * from edges;","columns":[{"name":"label","type":"TEXT"},{"name":"v","type":""}],"rows":[
["i64 max","9223372036854775807"],
["i64 min","-9223372036854775808"],
["tenth","0.1"],
["huge","1e300"],
["max f64","1.7976931348623157e308"],
["min subnormal","5e-324"],
["negative zero","-0.0"],
["infinity","inf"],
["invalid utf-8","\\xfffe41"],
["nul inside","a\u0000b"],
["emoji","bento 🍱"],
["quote and newline","say \"hi\"\nbye"],
["empty text",""],
["empty blob","\\x"],
["null",null]
],"row_count":15,"truncated":false,"rows_affected":null,"duration_ms":0,"status":"ok"},
{"index":7,"sql":"select * from generated;","columns":[{"name":"a","type":"INT"},{"name":"b","type":"INT"},{"name":"c","type":"TEXT"},{"name":"n","type":"TEXT"}],"rows":[
["21","42","BENTO","bento"]
],"row_count":1,"truncated":false,"rows_affected":null,"duration_ms":0,"status":"ok"},
{"index":8,"sql":"select * from v_declared;","columns":[{"name":"a","type":"VARCHAR(10)"},{"name":"d","type":"FLOATING POINT"},{"name":"e","type":"DECIMAL(10,2)"}],"rows":[
["longer than ten","2","1.1"],
["1","1.5","3"]
],"row_count":2,"truncated":false,"rows_affected":null,"duration_ms":0,"status":"ok"},
{"index":9,"sql":"select * from audited;","columns":[{"name":"v","type":"TEXT"}],"rows":[
["one"],
["two"]
],"row_count":2,"truncated":false,"rows_affected":null,"duration_ms":0,"status":"ok"},
{"index":10,"sql":"select * from audit_log;","columns":[{"name":"id","type":"INTEGER"},{"name":"msg","type":"TEXT"}],"rows":[
["1","inserted one"],
["2","inserted two"]
],"row_count":2,"truncated":false,"rows_affected":null,"duration_ms":0,"status":"ok"},
{"index":11,"sql":"select * from sqlite_sequence;","columns":[{"name":"name","type":""},{"name":"seq","type":""}],"rows":[
["audit_log","2"]
],"row_count":1,"truncated":false,"rows_affected":null,"duration_ms":0,"status":"ok"},
{"index":12,"sql":"select * from docs;","columns":[{"name":"title","type":""},{"name":"body","type":""}],"rows":[
["Bento","rice and fish"],
["Jubako","stacked boxes"]
],"row_count":2,"truncated":false,"rows_affected":null,"duration_ms":0,"status":"ok"},
{"index":13,"sql":"select * from docs_content;","columns":[{"name":"id","type":"INTEGER"},{"name":"c0","type":""},{"name":"c1","type":""}],"rows":[
["1","Bento","rice and fish"],
["2","Jubako","stacked boxes"]
],"row_count":2,"truncated":false,"rows_affected":null,"duration_ms":0,"status":"ok"},
{"index":14,"sql":"select * from docs_config;","columns":[{"name":"k","type":""},{"name":"v","type":""}],"rows":[
["version","4"]
],"row_count":1,"truncated":false,"rows_affected":null,"duration_ms":0,"status":"ok"}
]}
"#);
}

#[test]
fn a_3_mib_blob_arrives_whole_as_hex() {
    let ws = sqlite_ws("CREATE TABLE big_blob(b); INSERT INTO big_blob VALUES (zeroblob(3145728))");
    let (code, doc) = query_json(ws.path(), "select length(b), b from big_blob", &[]);
    assert_eq!(code, 0);
    let row = &doc["statements"][0]["rows"][0];
    assert_eq!(row[0], "3145728");
    let hex = row[1].as_str().unwrap();
    assert_eq!(hex.len(), 2 + 2 * 3145728);
    assert!(hex.starts_with("\\x0000") && hex[2..].bytes().all(|b| b == b'0'));
}

#[test]
fn a_wal_reader_is_not_blocked_by_an_open_writer() {
    let ws = sqlite_ws(FIXTURE);
    let writer = rusqlite::Connection::open(ws.path().join("data/app.db")).unwrap();
    writer
        .execute_batch("BEGIN IMMEDIATE; INSERT INTO audited VALUES ('pending')")
        .unwrap();
    let (code, doc) = query_json(ws.path(), "select count(*) from audited", &[]);
    assert_eq!(code, 0);
    assert_eq!(doc["statements"][0]["rows"][0][0], "2");
    writer.execute_batch("ROLLBACK").unwrap();
    assert_eq!(count(&ws, "select count(*) from audited"), 2);
}

#[test]
fn a_file_that_is_not_a_database_fails_the_statement() {
    let ws = sqlite_ws("");
    std::fs::write(ws.path().join("data/app.db"), "not a database ".repeat(20)).unwrap();
    let (code, doc) = query_json(ws.path(), "select 1", &[]);
    assert_eq!(code, 1);
    assert_eq!(doc["statements"][0]["error"]["code"], "26");
    assert_eq!(
        doc["statements"][0]["error"]["message"],
        "file is not a database"
    );
}

#[test]
fn a_writable_session_reads_an_attached_database() {
    let ws = sqlite_ws("");
    let other = ws.path().join("other.db");
    rusqlite::Connection::open(&other)
        .unwrap()
        .execute_batch("CREATE TABLE x(a TEXT); INSERT INTO x VALUES ('there')")
        .unwrap();
    let sql = format!("attach '{}' as o; select * from o.x", other.display());
    let (code, doc) = query_json(ws.path(), &sql, &[]);
    assert_eq!(code, 0);
    assert_eq!(doc["statements"][1]["rows"], serde_json::json!([["there"]]));
}

#[cfg(unix)]
#[test]
fn a_write_protected_file_is_a_read_only_violation() {
    use std::os::unix::fs::PermissionsExt;
    let ws = sqlite_ws("CREATE TABLE t(a)");
    let db = ws.path().join("data/app.db");
    std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o444)).unwrap();
    if std::fs::OpenOptions::new().write(true).open(&db).is_ok() {
        std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o644)).unwrap();
        eprintln!(
            "skipped: this user (root) can write a 0444 file, so file modes cannot be tested"
        );
        return;
    }
    let (code, doc) = query_json(ws.path(), "insert into t values (1)", &[]);
    std::fs::set_permissions(&db, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_eq!(code, 1);
    assert_eq!(doc["statements"][0]["error"]["code"], "read_only");
}
