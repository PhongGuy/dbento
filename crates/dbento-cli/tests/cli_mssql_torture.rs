//! The SQL Server type-torture fixture, loaded through `dbento run` and pinned as JSON.
mod common;
use common::{dbento, redact, write_tree};
use dbento_testkit::{MS_SECRET_VAR, MsDb, mssql_db};

const FIXTURE: &str = include_str!("fixtures/mssql_torture.sql");

/// A workspace on `db` (its stock certificate, so `tls = "off"`) with the fixture loaded
/// through `dbento run`.
fn torture_ws(db: &MsDb) -> tempfile::TempDir {
    let toml = format!("dbento = 1\n{}", db.connection_toml("ms", "tls = \"off\""));
    let ws = write_tree(&[
        ("dbento.toml", toml.as_str()),
        ("mssql_torture.sql", FIXTURE),
    ]);
    let out = dbento()
        .current_dir(ws.path())
        .env(MS_SECRET_VAR, db.password())
        .args([
            "run",
            "mssql_torture.sql",
            "--conn",
            "ms",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "loading failed: {}{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
    ws
}

/// `dbento query ms <sql> --format json` with durations redacted.
fn query(db: &MsDb, ws: &tempfile::TempDir, sql: &str) -> String {
    let out = dbento()
        .current_dir(ws.path())
        .env(MS_SECRET_VAR, db.password())
        .args(["query", "ms", sql, "--format", "json"])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    redact(&out.stdout)
}

#[tokio::test]
async fn every_type_family_arrives_as_the_values_table_says() {
    dbento_testkit::skip_without_mssql!();
    let db = mssql_db().await;
    let ws = torture_ws(&db);
    insta::assert_snapshot!(query(&db, &ws, "SELECT * FROM num_t ORDER BY id"), @r#"
{"dbento":1,"connection":"ms","statements":[
{"index":0,"sql":"SELECT * FROM num_t ORDER BY id","columns":[{"name":"id","type":"INT"},{"name":"b","type":"BIT"},{"name":"ti","type":"TINYINT"},{"name":"si","type":"SMALLINT"},{"name":"i","type":"INT"},{"name":"bi","type":"BIGINT"},{"name":"r","type":"REAL"},{"name":"f","type":"FLOAT"},{"name":"d","type":"DECIMAL(38,10)"},{"name":"m","type":"MONEY"},{"name":"sm","type":"SMALLMONEY"}],"rows":[
["1","0","0","-32768","-2147483648","-9223372036854775808","-3.4028235e+38","-1.7976931348623157e+308","-9999999999999999999999999999.9999999999","-922337203685477.5808","-214748.3648"],
["2","1","255","32767","2147483647","9223372036854775807","1.1754944e-38","2.2250738585072014e-308","0.0000000001","922337203685477.5807","214748.3647"],
["3","1","1","0","0","0","1.5","1e+20","0.0500000000","0.0001","0.0000"],
["4",null,null,null,null,null,null,null,null,null,null]
],"row_count":4,"truncated":false,"rows_affected":null,"duration_ms":0,"status":"ok"}
]}
"#);
    insta::assert_snapshot!(query(&db, &ws, "SELECT * FROM time_t ORDER BY id"), @r#"
{"dbento":1,"connection":"ms","statements":[
{"index":0,"sql":"SELECT * FROM time_t ORDER BY id","columns":[{"name":"id","type":"INT"},{"name":"d","type":"DATE"},{"name":"dt","type":"DATETIME"},{"name":"sdt","type":"SMALLDATETIME"},{"name":"dt2","type":"DATETIME2(7)"},{"name":"t","type":"TIME(7)"},{"name":"dto","type":"DATETIMEOFFSET(7)"},{"name":"t0","type":"TIME(0)"}],"rows":[
["1","0001-01-01","1753-01-01 00:00:00.000","1900-01-01 00:00:00.000","0001-01-01 00:00:00.0000000","00:00:00.0000000","0001-01-01 00:00:00.0000000 -14:00","00:00:00"],
["2","9999-12-31","9999-12-31 23:59:59.997","2079-06-06 23:59:00.000","9999-12-31 23:59:59.9999999","23:59:59.9999999","9999-12-31 23:59:59.9999999 +14:00","23:59:59"],
["3","2024-02-29","2024-02-29 12:34:56.790","2024-02-29 12:35:00.000","2024-02-29 12:34:56.1234567","12:34:56.5000000","2024-02-29 12:34:56.1234567 +05:30","12:34:56"],
["4",null,null,null,null,null,null,null]
],"row_count":4,"truncated":false,"rows_affected":null,"duration_ms":0,"status":"ok"}
]}
"#);
    insta::assert_snapshot!(query(&db, &ws, "SELECT * FROM text_t ORDER BY id"), @r#"
{"dbento":1,"connection":"ms","statements":[
{"index":0,"sql":"SELECT * FROM text_t ORDER BY id","columns":[{"name":"id","type":"INT"},{"name":"c","type":"CHAR(3)"},{"name":"v","type":"VARCHAR(20)"},{"name":"nv","type":"NVARCHAR(20)"},{"name":"nm","type":"NVARCHAR(MAX)"},{"name":"tx","type":"TEXT"},{"name":"ntx","type":"NTEXT"},{"name":"x","type":"XML"},{"name":"quoted Name ü","type":"NVARCHAR(10)"},{"name":"select","type":"NVARCHAR(10)"}],"rows":[
["1","ab ","café","bento 🍱","日本語 text","legacy","ünï","<a b=\"1\">t</a>","tab\tinside","it's"],
["2","   ","","","new\nline","","","<r/>","\"quoted\"","back\\slash"],
["3",null,null,null,null,null,null,null,null,null]
],"row_count":3,"truncated":false,"rows_affected":null,"duration_ms":0,"status":"ok"}
]}
"#);
    insta::assert_snapshot!(query(&db, &ws, "SELECT * FROM bin_t ORDER BY id"), @r#"
{"dbento":1,"connection":"ms","statements":[
{"index":0,"sql":"SELECT * FROM bin_t ORDER BY id","columns":[{"name":"id","type":"INT"},{"name":"b","type":"BINARY(3)"},{"name":"vb","type":"VARBINARY(10)"},{"name":"vm","type":"VARBINARY(MAX)"},{"name":"img","type":"IMAGE"},{"name":"g","type":"UNIQUEIDENTIFIER"},{"name":"h","type":"HIERARCHYID"}],"rows":[
["1","\\x00ff10","\\x","\\xc328","\\x02aa","6F9619FF-8B86-D011-B42D-00C04FC964FF","\\x5b40"],
["2","\\x616200","\\xfffe","\\x","\\x","00000000-0000-0000-0000-000000000000","\\x"],
["3",null,null,null,null,null,null]
],"row_count":3,"truncated":false,"rows_affected":null,"duration_ms":0,"status":"ok"}
]}
"#);
    insta::assert_snapshot!(query(&db, &ws, "SELECT * FROM variant_t ORDER BY id"), @r#"
{"dbento":1,"connection":"ms","statements":[
{"index":0,"sql":"SELECT * FROM variant_t ORDER BY id","columns":[{"name":"id","type":"INT"},{"name":"v","type":"SQL_VARIANT"}],"rows":[
["1","5"],
["2","1.50"],
["3","ab"],
["4","2024-01-02 03:04:05.12"],
["5","\\x0a"],
["6","1.0000"],
["7",null]
],"row_count":7,"truncated":false,"rows_affected":null,"duration_ms":0,"status":"ok"}
]}
"#);
}
