//! The Postgres type-torture fixture, pinned as JSON on every matrix server.
use crate::common::{dbento, redact, write_tree};
use dbento_testkit::{PgDb, SECRET_VAR, matrix, pg_db};

const FIXTURE: &str = include_str!("fixtures/pg_torture.sql");

/// A workspace on `db` with the fixture loaded through `dbento run`, one commit per statement.
fn torture_ws(label: &str, db: &PgDb) -> tempfile::TempDir {
    let toml = format!(
        "dbento = 1\n{}",
        db.connection_toml("pg", "settings = { TimeZone = \"Asia/Tokyo\" }")
    );
    let ws = write_tree(&[("dbento.toml", toml.as_str()), ("pg_torture.sql", FIXTURE)]);
    let out = dbento()
        .current_dir(ws.path())
        .env(SECRET_VAR, db.password())
        .args(["run", "pg_torture.sql", "--conn", "pg", "--format", "json"])
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

/// `dbento query pg <sql> --format json` with durations redacted.
fn query(label: &str, db: &PgDb, ws: &tempfile::TempDir, sql: &str) -> String {
    let out = dbento()
        .current_dir(ws.path())
        .env(SECRET_VAR, db.password())
        .args(["query", "pg", sql, "--format", "json"])
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

const SCALARS: &str = "SELECT * FROM num_t ORDER BY id;\nSELECT * FROM time_t ORDER BY id;\n\
SET TimeZone TO 'America/St_Johns';\nSELECT id, ts, tz, ttz FROM time_t ORDER BY id;\n\
SET IntervalStyle TO iso_8601;\nSELECT id, i FROM time_t ORDER BY id;\n\
SET IntervalStyle TO sql_standard;\nSELECT id, i FROM time_t ORDER BY id;\n\
SET IntervalStyle TO postgres_verbose;\nSELECT id, i FROM time_t ORDER BY id;\n\
SELECT * FROM text_t ORDER BY id;\nSELECT * FROM bin_t ORDER BY id";

#[tokio::test]
async fn scalar_families_arrive_as_the_servers_text() {
    // insta refuses a repeated inline assertion unless it is wrapped in allow_duplicates!,
    // which takes synchronous code: collect every server's output first.
    let servers = matrix().await;
    let outputs: Vec<(String, String)> = servers
        .iter()
        .map(|(label, db)| {
            let ws = torture_ws(label, db);
            (label.clone(), query(label, db, &ws, SCALARS))
        })
        .collect();
    insta::allow_duplicates! {
        for (label, json) in &outputs {
            eprintln!("checking {label}");
            insta::assert_snapshot!(json, @r#"
{"dbento":1,"connection":"pg","statements":[
{"index":0,"sql":"SELECT * FROM num_t ORDER BY id;","columns":[{"name":"id","type":"int4"},{"name":"s","type":"int2"},{"name":"i","type":"int4"},{"name":"b","type":"int8"},{"name":"n","type":"numeric"},{"name":"n38","type":"numeric"},{"name":"r","type":"float4"},{"name":"d","type":"float8"},{"name":"m","type":"money"}],"rows":[
["1","-32768","-2147483648","-9223372036854775808","12345678901234567890123456789012345678901234567890.5","1234567890123456789012345678.0123456789","NaN","-0","-$92,233,720,368,547,758.08"],
["2","32767","2147483647","9223372036854775807","NaN","-0.0000000001","1e-45","5e-324","$92,233,720,368,547,758.07"],
["3","0","0","0","Infinity","0.0000000000","-Infinity","Infinity","$0.00"],
["4","1","1","1","-Infinity","1.0000000000","3.4028235e+38","0.1","$1,234.50"],
["5",null,null,null,null,null,null,null,null]
],"row_count":5,"truncated":false,"rows_affected":null,"duration_ms":0,"status":"ok"},
{"index":1,"sql":"SELECT * FROM time_t ORDER BY id;","columns":[{"name":"id","type":"int4"},{"name":"d","type":"date"},{"name":"ts","type":"timestamp"},{"name":"tz","type":"timestamptz"},{"name":"t","type":"time"},{"name":"ttz","type":"timetz"},{"name":"i","type":"interval"}],"rows":[
["1","infinity","infinity","-infinity","24:00:00","00:00:00+14:59","1 year 2 mons 3 days 04:05:06.789012"],
["2","4713-01-01 BC","0001-01-01 00:00:00 BC","2024-03-10 21:00:00+09","23:59:59.999999","12:34:56-08","-1 years -2 mons +3 days -04:05:06"],
["3","2024-02-29","2024-02-29 23:59:59.123456","2000-01-01 13:59:59.5+09","00:00:00","00:00:00+00","00:00:00"],
["4",null,null,null,null,null,null]
],"row_count":4,"truncated":false,"rows_affected":null,"duration_ms":0,"status":"ok"},
{"index":2,"sql":"SET TimeZone TO 'America/St_Johns';","columns":[],"rows":[],"row_count":0,"truncated":false,"rows_affected":null,"duration_ms":0,"status":"ok"},
{"index":3,"sql":"SELECT id, ts, tz, ttz FROM time_t ORDER BY id;","columns":[{"name":"id","type":"int4"},{"name":"ts","type":"timestamp"},{"name":"tz","type":"timestamptz"},{"name":"ttz","type":"timetz"}],"rows":[
["1","infinity","-infinity","00:00:00+14:59"],
["2","0001-01-01 00:00:00 BC","2024-03-10 09:30:00-02:30","12:34:56-08"],
["3","2024-02-29 23:59:59.123456","2000-01-01 01:29:59.5-03:30","00:00:00+00"],
["4",null,null,null]
],"row_count":4,"truncated":false,"rows_affected":null,"duration_ms":0,"status":"ok"},
{"index":4,"sql":"SET IntervalStyle TO iso_8601;","columns":[],"rows":[],"row_count":0,"truncated":false,"rows_affected":null,"duration_ms":0,"status":"ok"},
{"index":5,"sql":"SELECT id, i FROM time_t ORDER BY id;","columns":[{"name":"id","type":"int4"},{"name":"i","type":"interval"}],"rows":[
["1","P1Y2M3DT4H5M6.789012S"],
["2","P-1Y-2M3DT-4H-5M-6S"],
["3","PT0S"],
["4",null]
],"row_count":4,"truncated":false,"rows_affected":null,"duration_ms":0,"status":"ok"},
{"index":6,"sql":"SET IntervalStyle TO sql_standard;","columns":[],"rows":[],"row_count":0,"truncated":false,"rows_affected":null,"duration_ms":0,"status":"ok"},
{"index":7,"sql":"SELECT id, i FROM time_t ORDER BY id;","columns":[{"name":"id","type":"int4"},{"name":"i","type":"interval"}],"rows":[
["1","+1-2 +3 +4:05:06.789012"],
["2","-1-2 +3 -4:05:06"],
["3","0"],
["4",null]
],"row_count":4,"truncated":false,"rows_affected":null,"duration_ms":0,"status":"ok"},
{"index":8,"sql":"SET IntervalStyle TO postgres_verbose;","columns":[],"rows":[],"row_count":0,"truncated":false,"rows_affected":null,"duration_ms":0,"status":"ok"},
{"index":9,"sql":"SELECT id, i FROM time_t ORDER BY id;","columns":[{"name":"id","type":"int4"},{"name":"i","type":"interval"}],"rows":[
["1","@ 1 year 2 mons 3 days 4 hours 5 mins 6.789012 secs"],
["2","@ 1 year 2 mons -3 days 4 hours 5 mins 6 secs ago"],
["3","@ 0"],
["4",null]
],"row_count":4,"truncated":false,"rows_affected":null,"duration_ms":0,"status":"ok"},
{"index":10,"sql":"SELECT * FROM text_t ORDER BY id;","columns":[{"name":"id","type":"int4"},{"name":"t","type":"text"},{"name":"c","type":"bpchar"},{"name":"v","type":"varchar"},{"name":"quoted Name ü","type":"text"},{"name":"select","type":"text"}],"rows":[
["1","","ab   ","bento 🍱","tab\tinside","reserved"],
["2","👨‍👩‍👧 family","é    ","مرحبا abc","שלום, world","new\nline"],
["3","日本語のテキスト","abcde","","\"quoted\"","back\\slash"],
["4",null,null,null,null,null]
],"row_count":4,"truncated":false,"rows_affected":null,"duration_ms":0,"status":"ok"},
{"index":11,"sql":"SELECT * FROM bin_t ORDER BY id","columns":[{"name":"id","type":"int4"},{"name":"b","type":"bytea"},{"name":"bits","type":"bit"},{"name":"vb","type":"varbit"}],"rows":[
["1","\\x00ff10","10100101","1"],
["2","\\x","00000000",""],
["3",null,null,null]
],"row_count":3,"truncated":false,"rows_affected":null,"duration_ms":0,"status":"ok"}
]}
"#);
        }
    }
}

const STRUCTURED: &str = "SELECT * FROM struct_t ORDER BY id;\nSELECT * FROM range_t ORDER BY id;\n\
SELECT * FROM net_t ORDER BY id;\nSELECT * FROM other_t ORDER BY id;\nSELECT * FROM shape_t ORDER BY id";

#[tokio::test]
async fn structured_families_arrive_as_the_servers_text() {
    // insta refuses a repeated inline assertion unless it is wrapped in allow_duplicates!,
    // which takes synchronous code: collect every server's output first.
    let servers = matrix().await;
    let outputs: Vec<(String, String)> = servers
        .iter()
        .map(|(label, db)| {
            let ws = torture_ws(label, db);
            (label.clone(), query(label, db, &ws, STRUCTURED))
        })
        .collect();
    insta::allow_duplicates! {
        for (label, json) in &outputs {
            eprintln!("checking {label}");
            insta::assert_snapshot!(json, @r#"
{"dbento":1,"connection":"pg","statements":[
{"index":0,"sql":"SELECT * FROM struct_t ORDER BY id;","columns":[{"name":"id","type":"int4"},{"name":"j","type":"json"},{"name":"jb","type":"jsonb"},{"name":"x","type":"xml"},{"name":"a1","type":"int4[]"},{"name":"a2","type":"text[]"},{"name":"a0","type":"int4[]"},{"name":"ac","type":"public.pair[]"},{"name":"p","type":"public.pair"},{"name":"m","type":"public.mood"},{"name":"dl","type":"int4[]"},{"name":"dp","type":"public.pair"},{"name":"pi","type":"int4"}],"rows":[
["1","{\"b\": 1, \"a\": 2, \"a\": 3}","{\"a\": 3, \"b\": 1}","<r a=\"1\">x &amp; y</r>","{1,NULL,3}","{{a,b},{c,NULL}}","[0:2]={1,2,3}","{\"(1,x)\",\"(,\\\"y \\\"\\\"q\\\"\\\"\\\")\"}","(1,\"a,b\")","meh","{4,5}","(2,)","7"],
["2","[[[[[[[[[[1]]]]]]]]]]","[[[[[[[[[[1]]]]]]]]]]","plain text","{}","{\"\",NULL}","[-1:-1]={0}","{}","(,)","happy","{}","(,\"\")","1"],
["3",null,null,null,null,null,null,null,null,null,null,null,null]
],"row_count":3,"truncated":false,"rows_affected":null,"duration_ms":0,"status":"ok"},
{"index":1,"sql":"SELECT * FROM range_t ORDER BY id;","columns":[{"name":"id","type":"int4"},{"name":"i4","type":"int4range"},{"name":"i8","type":"int8range"},{"name":"nr","type":"numrange"},{"name":"tsr","type":"tsrange"},{"name":"tstzr","type":"tstzrange"},{"name":"dr","type":"daterange"},{"name":"mr","type":"int4multirange"},{"name":"dmr","type":"datemultirange"}],"rows":[
["1","[1,10)","(,6)","empty","[\"2024-01-01 00:00:00\",)","[\"2024-01-01 09:00:00+09\",\"2024-01-02 09:00:00+09\")","[2024-01-01,2024-02-02)","{[1,3),[5,7)}","{}"],
["2","empty","(,)","[1.5,2.25]","(,)","empty","empty","{}","{[2024-01-01,2024-01-09)}"],
["3",null,null,null,null,null,null,null,null]
],"row_count":3,"truncated":false,"rows_affected":null,"duration_ms":0,"status":"ok"},
{"index":2,"sql":"SELECT * FROM net_t ORDER BY id;","columns":[{"name":"id","type":"int4"},{"name":"ip","type":"inet"},{"name":"c","type":"cidr"},{"name":"m","type":"macaddr"},{"name":"m8","type":"macaddr8"},{"name":"u","type":"uuid"}],"rows":[
["1","192.168.0.1/24","10.0.0.0/8","08:00:2b:01:02:03","08:00:2b:01:02:03:04:05","a0eebc99-9c0b-4ef8-bb6d-6bb9bd380a11"],
["2","::ffff:1.2.3.4","2001:db8::/32","ff:ff:ff:ff:ff:ff","08:00:2b:ff:fe:01:02:03","00000000-0000-0000-0000-000000000000"],
["3",null,null,null,null,null]
],"row_count":3,"truncated":false,"rows_affected":null,"duration_ms":0,"status":"ok"},
{"index":3,"sql":"SELECT * FROM other_t ORDER BY id;","columns":[{"name":"id","type":"int4"},{"name":"pt","type":"point"},{"name":"ln","type":"line"},{"name":"ls","type":"lseg"},{"name":"bx","type":"box"},{"name":"pa","type":"path"},{"name":"pg","type":"polygon"},{"name":"ci","type":"circle"},{"name":"tv","type":"tsvector"},{"name":"tq","type":"tsquery"},{"name":"lsn","type":"pg_lsn"},{"name":"o","type":"oid"},{"name":"rc","type":"regclass"},{"name":"rt","type":"regtype"},{"name":"snap","type":"pg_snapshot"},{"name":"ts","type":"txid_snapshot"},{"name":"ct","type":"public.citext"},{"name":"hs","type":"public.hstore"}],"rows":[
["1","(1.5,-2)","{1,-1,0}","[(0,0),(1,1)]","(1,1),(0,0)","[(0,0),(1,1),(2,0)]","((0,0),(1,1),(1,0))","<(0,0),5>","'a' 'cat':2A 'fat' 'sat'","'fat' & ( 'rat' | !'cat' )","16/B374D848","4294967295","pg_class","integer","10:20:10,14,15","10:20:10,14,15","MiXeD","\"a\"=>\"1\", \"b c\"=>NULL"],
["2",null,null,null,null,null,null,null,null,null,null,null,null,null,null,null,null,null]
],"row_count":2,"truncated":false,"rows_affected":null,"duration_ms":0,"status":"ok"},
{"index":4,"sql":"SELECT * FROM shape_t ORDER BY id","columns":[{"name":"id","type":"int4"},{"name":"a","type":"int4"},{"name":"g","type":"int4"}],"rows":[
["1","21","42"],
["2",null,null]
],"row_count":2,"truncated":false,"rows_affected":null,"duration_ms":0,"status":"ok"}
]}
"#);
        }
    }
}

// Money under a second lc_monetary is not covered: the alpine images ship no locales, and
// musl accepts any locale name while formatting money as in C.
const GAPS: &str = "SELECT 'NaN'::float8 AS d_nan, '{[1,3),[5,7)}'::int8multirange AS i8mr, \
'{[1.5,2.25],(3,4)}'::nummultirange AS nmr, \
'{[\"2024-01-01 00:00\",\"2024-01-02 00:00\"),[\"2024-03-01 12:00\",)}'::tsmultirange AS tsmr, \
'{[\"2024-01-01 00:00+00\",\"2024-01-02 00:00+00\")}'::tstzmultirange AS tstzmr, \
'e\u{301}a\u{308}\u{304} n\u{303}' AS combining, 'abc \u{5e9}\u{5dc}\u{5d5}\u{5dd} 123 def' AS bidi";

#[tokio::test]
async fn gap_families_arrive_as_the_servers_text() {
    let servers = matrix().await;
    let outputs: Vec<(String, String)> = servers
        .iter()
        .map(|(label, db)| {
            let toml = format!(
                "dbento = 1\n{}",
                db.connection_toml("pg", "settings = { TimeZone = \"Asia/Tokyo\" }")
            );
            let ws = write_tree(&[("dbento.toml", toml.as_str())]);
            (label.clone(), query(label, db, &ws, GAPS))
        })
        .collect();
    insta::allow_duplicates! {
        for (label, json) in &outputs {
            eprintln!("checking {label}");
            insta::assert_snapshot!(json, @r#"
{"dbento":1,"connection":"pg","statements":[
{"index":0,"sql":"SELECT 'NaN'::float8 AS d_nan, '{[1,3),[5,7)}'::int8multirange AS i8mr, '{[1.5,2.25],(3,4)}'::nummultirange AS nmr, '{[\"2024-01-01 00:00\",\"2024-01-02 00:00\"),[\"2024-03-01 12:00\",)}'::tsmultirange AS tsmr, '{[\"2024-01-01 00:00+00\",\"2024-01-02 00:00+00\")}'::tstzmultirange AS tstzmr, 'éǟ ñ' AS combining, 'abc שלום 123 def' AS bidi","columns":[{"name":"d_nan","type":"float8"},{"name":"i8mr","type":"int8multirange"},{"name":"nmr","type":"nummultirange"},{"name":"tsmr","type":"tsmultirange"},{"name":"tstzmr","type":"tstzmultirange"},{"name":"combining","type":"text"},{"name":"bidi","type":"text"}],"rows":[
["NaN","{[1,3),[5,7)}","{[1.5,2.25],(3,4)}","{[\"2024-01-01 00:00:00\",\"2024-01-02 00:00:00\"),[\"2024-03-01 12:00:00\",)}","{[\"2024-01-01 09:00:00+09\",\"2024-01-02 09:00:00+09\")}","éǟ ñ","abc שלום 123 def"]
],"row_count":1,"truncated":false,"rows_affected":null,"duration_ms":0,"status":"ok"}
]}
"#);
        }
    }
}

#[tokio::test]
async fn a_json_document_of_several_mib_arrives_whole() {
    let db = pg_db().await;
    let toml = format!("dbento = 1\n{}", db.connection_toml("pg", ""));
    let ws = write_tree(&[("dbento.toml", toml.as_str())]);
    let sql = "SELECT length(j::text), j FROM (SELECT json_build_object('k', repeat('x', 6 * 1024 * 1024), \
               'n', json_build_array(1, 'two', null)) AS j) s";
    let (code, doc) = json_of("default", &db, &ws, sql);
    assert_eq!(code, 0);
    let row = &doc["statements"][0]["rows"][0];
    let text = row[1].as_str().unwrap();
    assert_eq!(row[0].as_str().unwrap(), text.len().to_string());
    let value: serde_json::Value = serde_json::from_str(text).unwrap();
    assert_eq!(value["k"].as_str().unwrap().len(), 6 * 1024 * 1024);
    assert_eq!(value["n"], serde_json::json!([1, "two", null]));
}

fn json_of(label: &str, db: &PgDb, ws: &tempfile::TempDir, sql: &str) -> (i32, serde_json::Value) {
    let out = dbento()
        .current_dir(ws.path())
        .env(SECRET_VAR, db.password())
        .args(["query", "pg", sql, "--format", "json"])
        .output()
        .unwrap();
    let doc = serde_json::from_slice(&out.stdout).unwrap_or_else(|e| panic!("{label}: {e}"));
    (out.status.code().unwrap(), doc)
}

#[tokio::test]
async fn values_past_the_batch_cap_arrive_whole() {
    let db = pg_db().await;
    let toml = format!("dbento = 1\n{}", db.connection_toml("pg", ""));
    let ws = write_tree(&[("dbento.toml", toml.as_str())]);
    let sql = "SELECT length(t), t FROM (SELECT repeat('x', 16 * 1024 * 1024) AS t) s;\n\
               SELECT length(b), b FROM (SELECT decode(repeat('ab', 9 * 1024 * 1024), 'hex') AS b) s;\n\
               SELECT ('1' || repeat('0', 1000) || '.5')::numeric";
    let (code, doc) = json_of("default", &db, &ws, sql);
    assert_eq!(code, 0);
    let s = &doc["statements"];
    assert_eq!(s[0]["rows"][0][0], "16777216");
    let t = s[0]["rows"][0][1].as_str().unwrap();
    assert!(t.len() == 16 * 1024 * 1024 && t.bytes().all(|b| b == b'x'));
    assert_eq!(s[1]["rows"][0][0], "9437184");
    let b = s[1]["rows"][0][1].as_str().unwrap();
    assert!(b.len() == 2 + 2 * 9437184 && b.starts_with("\\xabab"));
    let n = s[2]["rows"][0][0].as_str().unwrap();
    assert!(
        n.len() == 1003 && n.starts_with("10") && n.ends_with("0.5"),
        "{}",
        &n[..20]
    );
}

#[tokio::test]
async fn server_limits_fail_with_the_servers_code() {
    let db = pg_db().await;
    let cols: Vec<String> = (1..=1600).map(|i| format!("c{i} int")).collect();
    db.admin()
        .await
        .batch_execute(&format!(
            "CREATE TABLE wide({}); INSERT INTO wide (c1, c1600) VALUES (1, 1600)",
            cols.join(", ")
        ))
        .await
        .unwrap();
    let toml = format!("dbento = 1\n{}", db.connection_toml("pg", ""));
    let ws = write_tree(&[("dbento.toml", toml.as_str())]);
    let (code, doc) = json_of("default", &db, &ws, "SELECT * FROM wide");
    assert_eq!(code, 0);
    let s = &doc["statements"][0];
    assert_eq!(s["columns"].as_array().unwrap().len(), 1600);
    assert_eq!(s["rows"][0][0], "1");
    assert!(s["rows"][0][1].is_null());
    assert_eq!(s["rows"][0][1599], "1600");
    let targets: Vec<String> = (1..=1665).map(|i| i.to_string()).collect();
    let (code, doc) = json_of(
        "default",
        &db,
        &ws,
        &format!("SELECT {}", targets.join(", ")),
    );
    assert_eq!(code, 1);
    assert_eq!(doc["statements"][0]["error"]["code"], "54011");
    let (code, doc) = json_of("default", &db, &ws, "SELECT chr(0)");
    assert_eq!(code, 1);
    assert_eq!(doc["statements"][0]["error"]["code"], "54000");
}
