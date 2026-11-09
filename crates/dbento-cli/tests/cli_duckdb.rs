//! `dbento query` against a DuckDB workspace, and what a second process sees while one
//! process holds the file.
#![cfg(feature = "duckdb")]
use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use crate::common::{dbento, write_tree};
use serde_json::{Value, json};
use tempfile::TempDir;

const DUCK_TOML: &str =
    "dbento = 1\n[connections.duck]\nengine = \"duckdb\"\npath = \"data/app.duckdb\"\n";

/// Cannot finish within any test bound: 10^13 rows through a cross join.
const SPIN: &str = "SELECT count(*) FROM range(10000000) a, range(1000000) b";

/// A workspace whose `duck` connection is `data/app.duckdb`, built by running `setup` with
/// the duckdb crate (DuckDB rejects an empty file, so the file must be a real database).
fn duck_ws(setup: &str) -> TempDir {
    let ws = write_tree(&[("dbento.toml", DUCK_TOML)]);
    std::fs::create_dir(ws.path().join("data")).unwrap();
    dbento_testkit::duckdb_file(&ws.path().join("data/app.duckdb"), setup);
    ws
}

/// `dbento query duck <sql> --format json <extra>`: exit code and the parsed document.
fn query_json(ws: &TempDir, sql: &str, extra: &[&str]) -> (Option<i32>, Value) {
    let out = dbento()
        .current_dir(ws.path())
        .args(["query", "duck", sql, "--format", "json"])
        .args(extra)
        .output()
        .unwrap();
    let doc = serde_json::from_slice(&out.stdout).unwrap_or(Value::Null);
    (out.status.code(), doc)
}

/// A second `dbento` that holds the file while it runs SPIN; killed when dropped.
struct Holder(Child);

impl Holder {
    /// Returns only once the holder has opened the file: the JSON sink writes its first
    /// line after connect, so waiting for that line removes any race with the test's open.
    fn start(ws: &TempDir, extra: &[&str]) -> Holder {
        let mut child = Command::new(env!("CARGO_BIN_EXE_dbento"))
            .current_dir(ws.path())
            .args(["query", "duck", SPIN, "--format", "json"])
            .args(extra)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut first = String::new();
        BufReader::new(child.stdout.as_mut().unwrap())
            .read_line(&mut first)
            .unwrap();
        assert!(
            first.starts_with("{\"dbento\":1"),
            "holder never opened: {first:?}"
        );
        Holder(child)
    }
}

impl Drop for Holder {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Runs `dbento query` once against a file the holder already has open; it must be refused
/// with exit 3, and its stderr is returned.
fn refused_by_the_holder(ws: &TempDir, extra: &[&str]) -> String {
    let out = dbento()
        .current_dir(ws.path())
        .args(["query", "duck", "SELECT 1"])
        .args(extra)
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert_eq!(out.status.code(), Some(3), "{stderr}");
    assert!(stderr.contains("in another process"), "{stderr}");
    stderr
}

#[test]
fn query_prints_a_duckdb_result_with_nested_values() {
    let ws = duck_ws(
        "CREATE TABLE t(a INTEGER, l INTEGER[], s STRUCT(k VARCHAR)); \
         INSERT INTO t VALUES (1, [1, 2], {'k': 'x y'}), (2, NULL, NULL)",
    );
    let (code, doc) = query_json(&ws, "SELECT a, l, s FROM t ORDER BY a", &[]);
    assert_eq!(code, Some(0), "{doc}");
    let s = &doc["statements"][0];
    assert_eq!(
        s["columns"],
        json!([
            {"name": "a", "type": "INTEGER"},
            {"name": "l", "type": "INTEGER[]"},
            {"name": "s", "type": "STRUCT(k VARCHAR)"}
        ])
    );
    assert_eq!(
        s["rows"],
        json!([["1", "[1, 2]", "{'k': x y}"], ["2", null, null]])
    );
}

#[test]
fn the_timeout_flag_stops_a_duckdb_statement() {
    let ws = duck_ws("SELECT 1");
    let started = Instant::now();
    let (code, doc) = query_json(&ws, SPIN, &["--timeout", "200ms"]);
    assert!(started.elapsed() < Duration::from_secs(10));
    assert_eq!(code, Some(1), "{doc}");
    assert_eq!(doc["statements"][0]["error"]["code"], "timeout");
}

#[test]
fn a_file_held_read_write_by_another_process_is_refused_with_dbentos_message() {
    let ws = duck_ws("SELECT 1");
    let _holder = Holder::start(&ws, &[]);
    let stderr = refused_by_the_holder(&ws, &["--read-only"]);
    assert!(
        stderr.contains("is open read-write in another process (for example the Dbento app)"),
        "{stderr}"
    );
}

#[test]
fn a_file_held_read_only_by_another_process_refuses_a_writer() {
    let ws = duck_ws("SELECT 1");
    let _holder = Holder::start(&ws, &["--read-only"]);
    let stderr = refused_by_the_holder(&ws, &[]);
    assert!(
        stderr.contains("is open read-only in another process"),
        "{stderr}"
    );
}

#[test]
fn a_missing_duckdb_file_is_exit_3_and_stays_missing() {
    let ws = write_tree(&[("dbento.toml", DUCK_TOML)]);
    let (code, doc) = query_json(&ws, "SELECT 1", &[]);
    assert_eq!((code, doc), (Some(3), Value::Null));
    assert!(!ws.path().join("data/app.duckdb").exists());
}
