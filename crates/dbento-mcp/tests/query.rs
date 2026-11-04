//! The query tool: one statement, read-only, row and byte bounds and cut cells.
mod common;
use common::TestServer;
use dbento_testkit::pg_db;
use serde_json::{Value, json};

const T: &str = "CREATE TABLE t(id INTEGER, note TEXT, b BLOB); INSERT INTO t VALUES (1, 'a', x'00ff'), (2, NULL, NULL);";
/// 5,000 rows of small integers.
const MANY: &str = "WITH RECURSIVE r(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM r WHERE x < 5000) SELECT x FROM r";
/// 5,000 rows of exactly 1,000 bytes.
const KILOS: &str = "WITH RECURSIVE r(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM r WHERE x < 5000) SELECT printf('%01000d', x) FROM r";

fn q(sql: &str) -> Value {
    json!({"connection": "lite", "sql": sql})
}

fn bounds(r: &Value) -> (Value, Value, Value) {
    (
        r["row_count"].clone(),
        r["truncated"].clone(),
        r["truncated_by"].clone(),
    )
}

#[tokio::test]
async fn rows_come_back_as_text_null_and_hex_cells() {
    let server = TestServer::sqlite(T, "").await;
    let r = server
        .ok("query", q("SELECT id, note, b FROM t ORDER BY id"))
        .await;
    assert_eq!(
        r["columns"],
        json!([{"name": "id", "type": "INTEGER"}, {"name": "note", "type": "TEXT"}, {"name": "b", "type": "BLOB"}])
    );
    assert_eq!(
        r["rows"],
        json!([["1", "a", {"hex": "00ff"}], ["2", null, null]])
    );
    assert_eq!(bounds(&r), (json!(2), json!(false), Value::Null));
    assert!(r["duration_ms"].is_u64(), "{r}");
    // A trailing semicolon is still one statement.
    assert_eq!(
        server.ok("query", q("SELECT 1;")).await["rows"],
        json!([["1"]])
    );
}

#[tokio::test]
async fn max_rows_defaults_to_100_and_is_capped_at_1000() {
    let server = TestServer::sqlite("", "").await;
    let r = server.ok("query", q(MANY)).await;
    assert_eq!(bounds(&r), (json!(100), json!(true), json!("rows")));
    assert_eq!(r["rows"].as_array().unwrap().len(), 100);
    let r = server
        .ok(
            "query",
            json!({"connection": "lite", "sql": MANY, "max_rows": 3}),
        )
        .await;
    assert_eq!(r["rows"], json!([["1"], ["2"], ["3"]]));
    let r = server
        .ok(
            "query",
            json!({"connection": "lite", "sql": MANY, "max_rows": 5000}),
        )
        .await;
    assert_eq!(bounds(&r), (json!(1000), json!(true), json!("rows")));
}

#[tokio::test]
async fn the_byte_budget_stops_a_wide_result_at_64_kib() {
    let server = TestServer::sqlite("", "").await;
    let r = server
        .ok(
            "query",
            json!({"connection": "lite", "sql": KILOS, "max_rows": 1000}),
        )
        .await;
    // 65 rows hold 65,000 bytes, under 65,536, so a 66th joins and the 67th is cut.
    assert_eq!(bounds(&r), (json!(66), json!(true), json!("bytes")));
}

#[tokio::test]
async fn long_cells_are_cut_at_4_kib_on_a_char_boundary() {
    let server = TestServer::sqlite("", "").await;
    let sql = "SELECT replace(printf('%05000d', 0), '0', 'x'), replace(printf('%04095d', 0), '0', 'a') || 'é', printf('%04096d', 0), zeroblob(3000)";
    let row = server.ok("query", q(sql)).await["rows"][0].clone();
    assert_eq!(row[0], json!({"text": "x".repeat(4096), "cut": true}));
    assert_eq!(row[1], json!({"text": "a".repeat(4095), "cut": true}));
    assert_eq!(row[2], json!("0".repeat(4096)), "exactly 4 KiB is not cut");
    assert_eq!(row[3], json!({"hex": "0".repeat(4096), "cut": true}));
}

#[tokio::test]
async fn anything_but_one_statement_is_refused_before_connecting() {
    // `gone` is a missing file, so a connect would answer no_connection instead.
    let server = TestServer::start(
        "dbento = 1\n[connections.gone]\nengine = \"sqlite\"\npath = \"gone.db\"\n",
    )
    .await;
    for sql in [
        "SELECT 1; SELECT 2",
        "",
        "   ",
        "SELECT 1; COMMIT; DROP TABLE t",
    ] {
        let e = server
            .err("query", json!({"connection": "gone", "sql": sql}))
            .await;
        assert_eq!(e["code"], "one_statement", "{sql:?}: {e}");
    }
    let e = server
        .err("query", json!({"connection": "gone", "sql": "SELECT 1"}))
        .await;
    assert_eq!(e["code"], "no_connection");
}

#[tokio::test]
async fn a_write_is_refused_by_the_database_and_errors_keep_their_code() {
    let server = TestServer::sqlite(T, "").await;
    let e = server
        .err("query", q("INSERT INTO t VALUES (3, 'c', NULL)"))
        .await;
    assert_eq!(e["code"], "read_only", "{e}");
    assert!(e["message"].as_str().unwrap().contains("execute"), "{e}");
    let count: i64 = rusqlite::Connection::open(server.root().join("data/app.db"))
        .unwrap()
        .query_row("SELECT count(*) FROM t", [], |r| r.get(0))
        .unwrap();
    assert_eq!(count, 2);
    let e = server.err("query", q("SELECT * FROM nope")).await;
    assert_eq!(
        (e["code"].clone(), e["sqlstate"].clone()),
        (json!("database"), json!("1"))
    );
    assert!(
        e["message"]
            .as_str()
            .unwrap()
            .contains("no such table: nope"),
        "{e}"
    );
}

#[tokio::test]
async fn postgres_types_read_only_and_transaction_control() {
    let db = pg_db().await;
    let server = TestServer::postgres(&db, "").await;
    let pg = |sql: &str| json!({"connection": "pg", "sql": sql});
    let r = server
        .ok("query", pg("SELECT 1::int8 AS a, 'x'::text AS b, '\\x00ff'::bytea AS c, NULL::text AS d, '{\"k\":1}'::jsonb AS e"))
        .await;
    assert_eq!(
        r["columns"],
        json!([{"name": "a", "type": "int8"}, {"name": "b", "type": "text"}, {"name": "c", "type": "bytea"},
               {"name": "d", "type": "text"}, {"name": "e", "type": "jsonb"}])
    );
    assert_eq!(
        r["rows"],
        json!([["1", "x", "\\x00ff", null, "{\"k\": 1}"]])
    );
    assert_eq!(
        server.err("query", pg("CREATE TABLE z(a int)")).await["code"],
        "read_only"
    );
    assert_eq!(
        server.err("query", pg("COMMIT")).await["code"],
        "transaction_control"
    );
    assert_eq!(
        server.err("query", pg("SELECT 1; SELECT 2")).await["code"],
        "one_statement"
    );
    let e = server.err("query", pg("SELECT * FROM nope")).await;
    assert_eq!(
        (e["code"].clone(), e["sqlstate"].clone()),
        (json!("database"), json!("42P01"))
    );
    assert_eq!(
        db.scalar("SELECT to_regclass('z') IS NULL")
            .await
            .as_deref(),
        Some("t")
    );
}

#[tokio::test]
async fn postgres_bytes_stop_at_the_budget() {
    let db = pg_db().await;
    let server = TestServer::postgres(&db, "").await;
    let sql = "SELECT repeat('x', 1000) FROM generate_series(1, 5000)";
    let r = server
        .ok(
            "query",
            json!({"connection": "pg", "sql": sql, "max_rows": 1000}),
        )
        .await;
    assert_eq!(bounds(&r), (json!(66), json!(true), json!("bytes")));
}

#[tokio::test]
async fn the_wide_stress_table_stops_at_the_byte_budget() {
    let Some(path) = std::env::var_os("DBENTO_STRESS_SQLITE") else {
        eprintln!("skipped: DBENTO_STRESS_SQLITE is not set");
        return;
    };
    let path = path.into_string().expect("a UTF-8 path");
    let toml = format!("dbento = 1\n[connections.stress]\nengine = \"sqlite\"\npath = {path:?}\n");
    let server = TestServer::start(&toml).await;
    let r = server
        .ok(
            "query",
            json!({"connection": "stress", "sql": "SELECT * FROM wide", "max_rows": 1000}),
        )
        .await;
    assert_eq!(
        (r["truncated"].clone(), r["truncated_by"].clone()),
        (json!(true), json!("bytes"))
    );
    assert!(
        r["row_count"].as_u64().unwrap() < 1000,
        "{}",
        r["row_count"]
    );
}
