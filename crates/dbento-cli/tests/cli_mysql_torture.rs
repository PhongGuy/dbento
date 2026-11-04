//! The MySQL type-torture fixture, pinned as JSON on every MySQL-family matrix server.
mod common;
use common::{dbento, redact, write_tree};
use dbento_testkit::{MY_SECRET_VAR, MyDb, MyFlavor, my_matrix};

const FIXTURE: &str = include_str!("fixtures/mysql_torture.sql");

/// A workspace on `db` with the fixture loaded through `dbento run`.
fn torture_ws(label: &str, db: &MyDb) -> tempfile::TempDir {
    let toml = format!("dbento = 1\n{}", db.connection_toml("my", ""));
    let ws = write_tree(&[
        ("dbento.toml", toml.as_str()),
        ("mysql_torture.sql", FIXTURE),
    ]);
    let out = dbento()
        .current_dir(ws.path())
        .env(MY_SECRET_VAR, db.password())
        .args([
            "run",
            "mysql_torture.sql",
            "--conn",
            "my",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{label}: loading failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    ws
}

/// `dbento query my <sql> --format json` with durations redacted.
fn query(label: &str, db: &MyDb, ws: &tempfile::TempDir, sql: &str) -> String {
    let out = dbento()
        .current_dir(ws.path())
        .env(MY_SECRET_VAR, db.password())
        .args(["query", "my", sql, "--format", "json"])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{label}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    redact(&out.stdout)
}

const SHARED: &str = "SET time_zone = '+00:00';\nSELECT * FROM num_t ORDER BY id;\nSELECT * FROM time_t ORDER BY id;\n\
SELECT * FROM text_t ORDER BY id;\nSELECT * FROM bin_t ORDER BY id";

const FLAVORED: &str = "SET time_zone = '+00:00';\nSELECT * FROM json_t ORDER BY id;\n\
SELECT X'FF' AS raw, CAST(X'C328' AS CHAR CHARACTER SET latin1) AS l1, 1.5e0 AS dbl, CAST(1 AS UNSIGNED) AS u, \
NULL AS n, b'101' AS bitlit, JSON_OBJECT('k', 1) AS jo";

#[tokio::test]
async fn scalar_families_arrive_alike_on_every_flavor() {
    // insta refuses a repeated inline assertion unless it is wrapped in allow_duplicates!,
    // which takes synchronous code: collect every server's output first.
    let servers = my_matrix().await;
    let outputs: Vec<(String, String)> = servers
        .iter()
        .map(|(label, db)| {
            let ws = torture_ws(label, db);
            (label.clone(), query(label, db, &ws, SHARED))
        })
        .collect();
    insta::allow_duplicates! {
        for (label, json) in &outputs {
            eprintln!("checking {label}");
            insta::assert_snapshot!(json, @r#"
{"dbento":1,"connection":"my","statements":[
{"index":0,"sql":"SET time_zone = '+00:00'","columns":[],"rows":[],"row_count":0,"truncated":false,"rows_affected":null,"duration_ms":0,"status":"ok"},
{"index":1,"sql":"SELECT * FROM num_t ORDER BY id","columns":[{"name":"id","type":"INT"},{"name":"ti","type":"TINYINT"},{"name":"si","type":"SMALLINT UNSIGNED"},{"name":"mi","type":"MEDIUMINT"},{"name":"i","type":"INT"},{"name":"bi","type":"BIGINT"},{"name":"ub","type":"BIGINT UNSIGNED"},{"name":"d","type":"DECIMAL"},{"name":"f","type":"FLOAT"},{"name":"db","type":"DOUBLE"},{"name":"y","type":"YEAR"}],"rows":[
["1","-128","0","-8388608","-2147483648","-9223372036854775808","0","-12345678901234567890123456789012345.123456789012345678901234567890","-3.40282e38","-1.7976931348623157e308","1901"],
["2","127","65535","8388607","2147483647","9223372036854775807","18446744073709551615","0.000000000000000000000000000001","1.17549e-38","2.2250738585072014e-308","2155"],
["3","0","1","0","0","0","1","0.000000000000000000000000000000","0","0","0000"],
["4",null,null,null,null,null,null,null,null,null,null]
],"row_count":4,"truncated":false,"rows_affected":null,"duration_ms":0,"status":"ok"},
{"index":2,"sql":"SELECT * FROM time_t ORDER BY id","columns":[{"name":"id","type":"INT"},{"name":"d","type":"DATE"},{"name":"dt","type":"DATETIME"},{"name":"ts","type":"TIMESTAMP"},{"name":"t","type":"TIME"}],"rows":[
["1","0000-00-00","0000-00-00 00:00:00.000000","1970-01-01 00:00:01.000","-838:59:59.0"],
["2","2020-00-00","9999-12-31 23:59:59.999999","2038-01-19 03:14:07.999","838:59:59.0"],
["3","2024-02-29","1000-01-01 00:00:00.000001","2024-02-29 12:34:56.789","00:00:00.5"],
["4",null,null,null,null]
],"row_count":4,"truncated":false,"rows_affected":null,"duration_ms":0,"status":"ok"},
{"index":3,"sql":"SELECT * FROM text_t ORDER BY id","columns":[{"name":"id","type":"INT"},{"name":"c","type":"CHAR"},{"name":"v","type":"VARCHAR"},{"name":"t","type":"TEXT"},{"name":"quoted Name ü","type":"VARCHAR"},{"name":"select","type":"VARCHAR"},{"name":"e","type":"ENUM"},{"name":"s","type":"SET"}],"rows":[
["1","ab","bento 🍱","","tab\tinside","back\\slash","small","a,c"],
["2","é","日本語 text","new\nline","\"quoted\"","it's","large",""],
["3",null,null,null,null,null,null,null]
],"row_count":3,"truncated":false,"rows_affected":null,"duration_ms":0,"status":"ok"},
{"index":4,"sql":"SELECT * FROM bin_t ORDER BY id","columns":[{"name":"id","type":"INT"},{"name":"b","type":"BINARY"},{"name":"vb","type":"VARBINARY"},{"name":"bl","type":"BLOB"},{"name":"bits","type":"BIT"},{"name":"g","type":"GEOMETRY"}],"rows":[
["1","\\x00ff10","\\x","\\xc328","\\x02aa","\\x000000000101000000000000000000f03f0000000000000040"],
["2","\\x616263","\\xfffe","\\x","\\x0000",null],
["3",null,null,null,null,null]
],"row_count":3,"truncated":false,"rows_affected":null,"duration_ms":0,"status":"ok"}
]}
"#);
        }
    }
}

#[tokio::test]
async fn json_and_literal_types_differ_by_flavor() {
    let servers = my_matrix().await;
    let outputs: Vec<(String, MyFlavor, String)> = servers
        .iter()
        .map(|(label, db)| {
            let ws = torture_ws(label, db);
            (label.clone(), db.flavor(), query(label, db, &ws, FLAVORED))
        })
        .collect();
    insta::allow_duplicates! {
        for (label, _, json) in outputs.iter().filter(|(_, f, _)| *f == MyFlavor::Mysql) {
            eprintln!("checking {label}");
            insta::assert_snapshot!(json, @r#"
{"dbento":1,"connection":"my","statements":[
{"index":0,"sql":"SET time_zone = '+00:00'","columns":[],"rows":[],"row_count":0,"truncated":false,"rows_affected":null,"duration_ms":0,"status":"ok"},
{"index":1,"sql":"SELECT * FROM json_t ORDER BY id","columns":[{"name":"id","type":"INT"},{"name":"j","type":"JSON"}],"rows":[
["1","{\"a\": [1, \"two\", null], \"b\": 1}"],
["2","\"text\""],
["3",null]
],"row_count":3,"truncated":false,"rows_affected":null,"duration_ms":0,"status":"ok"},
{"index":2,"sql":"SELECT X'FF' AS raw, CAST(X'C328' AS CHAR CHARACTER SET latin1) AS l1, 1.5e0 AS dbl, CAST(1 AS UNSIGNED) AS u, NULL AS n, b'101' AS bitlit, JSON_OBJECT('k', 1) AS jo","columns":[{"name":"raw","type":"VARBINARY"},{"name":"l1","type":"VARCHAR"},{"name":"dbl","type":"DOUBLE"},{"name":"u","type":"BIGINT UNSIGNED"},{"name":"n","type":"NULL"},{"name":"bitlit","type":"VARBINARY"},{"name":"jo","type":"JSON"}],"rows":[
["\\xff","Ã(","1.5","1",null,"\\x05","{\"k\": 1}"]
],"row_count":1,"truncated":false,"rows_affected":null,"duration_ms":0,"status":"ok"}
]}
"#);
        }
    }
    insta::allow_duplicates! {
        for (label, _, json) in outputs.iter().filter(|(_, f, _)| *f == MyFlavor::Mariadb) {
            eprintln!("checking {label}");
            insta::assert_snapshot!(json, @r#"
{"dbento":1,"connection":"my","statements":[
{"index":0,"sql":"SET time_zone = '+00:00'","columns":[],"rows":[],"row_count":0,"truncated":false,"rows_affected":null,"duration_ms":0,"status":"ok"},
{"index":1,"sql":"SELECT * FROM json_t ORDER BY id","columns":[{"name":"id","type":"INT"},{"name":"j","type":"TEXT"}],"rows":[
["1","{\"b\": 1, \"a\": [1, \"two\", null]}"],
["2","\"text\""],
["3",null]
],"row_count":3,"truncated":false,"rows_affected":null,"duration_ms":0,"status":"ok"},
{"index":2,"sql":"SELECT X'FF' AS raw, CAST(X'C328' AS CHAR CHARACTER SET latin1) AS l1, 1.5e0 AS dbl, CAST(1 AS UNSIGNED) AS u, NULL AS n, b'101' AS bitlit, JSON_OBJECT('k', 1) AS jo","columns":[{"name":"raw","type":"VARBINARY"},{"name":"l1","type":"VARCHAR"},{"name":"dbl","type":"DOUBLE"},{"name":"u","type":"INT UNSIGNED"},{"name":"n","type":"NULL"},{"name":"bitlit","type":"VARBINARY"},{"name":"jo","type":"VARCHAR"}],"rows":[
["\\xff","Ã(","1.5","1",null,"\\x05","{\"k\": 1}"]
],"row_count":1,"truncated":false,"rows_affected":null,"duration_ms":0,"status":"ok"}
]}
"#);
        }
    }
}
