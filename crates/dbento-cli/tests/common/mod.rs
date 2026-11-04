//! Helpers shared by the CLI integration tests.
#![allow(dead_code)] // each test binary uses a different subset

use std::path::Path;
use tempfile::TempDir;

/// 5,000 rows.
pub const MANY: &str = "WITH RECURSIVE r(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM r WHERE x < 5000) SELECT x FROM r";
/// Never finishes on its own.
pub const LONG: &str =
    "WITH RECURSIVE r(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM r) SELECT count(*) FROM r";

/// The built `dbento`, with the TTY seam cleared.
pub fn dbento() -> assert_cmd::Command {
    let mut cmd = assert_cmd::Command::cargo_bin("dbento").expect("binary");
    cmd.env_remove("DBENTO_TEST_ASSUME_TTY");
    cmd
}

/// Writes `files` (relative path, contents) under a new temporary directory.
pub fn write_tree(files: &[(&str, &str)]) -> TempDir {
    let dir = TempDir::new().expect("tempdir");
    for (rel, text) in files {
        let path = dir.path().join(rel);
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, text).expect("write");
    }
    dir
}

pub const LITE_TOML: &str =
    "dbento = 1\n[connections.lite]\nengine = \"sqlite\"\npath = \"data/app.db\"\n";

/// A workspace whose connection `lite` is `data/app.db`, created by running `setup`.
pub fn sqlite_ws(setup: &str) -> TempDir {
    let dir = write_tree(&[("dbento.toml", LITE_TOML)]);
    std::fs::create_dir(dir.path().join("data")).expect("mkdir");
    rusqlite::Connection::open(dir.path().join("data/app.db"))
        .expect("open")
        .execute_batch(setup)
        .expect("setup");
    dir
}

/// A scalar from `data/app.db` through its own connection.
pub fn count(ws: &TempDir, sql: &str) -> i64 {
    let c = rusqlite::Connection::open(ws.path().join("data/app.db")).expect("open");
    c.query_row(sql, [], |r| r.get(0)).expect("scalar")
}

/// `dbento query lite <sql> --format json <extra>` in `ws`: exit code and parsed stdout (Null if empty).
pub fn query_json(ws: &Path, sql: &str, extra: &[&str]) -> (i32, serde_json::Value) {
    let out = dbento()
        .current_dir(ws)
        .args(["query", "lite", sql, "--format", "json"])
        .args(extra)
        .output()
        .expect("runs");
    let doc = if out.stdout.is_empty() {
        serde_json::Value::Null
    } else {
        serde_json::from_slice(&out.stdout)
            .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stdout)))
    };
    (out.status.code().expect("exit code"), doc)
}

/// stdout as text with every `"duration_ms":<n>` replaced by `"duration_ms":0`.
pub fn redact(stdout: &[u8]) -> String {
    let text = String::from_utf8(stdout.to_vec()).expect("utf-8 stdout");
    let key = "\"duration_ms\":";
    let (mut out, mut rest) = (String::new(), text.as_str());
    while let Some(at) = rest.find(key) {
        out.push_str(&rest[..at + key.len()]);
        out.push('0');
        rest = rest[at + key.len()..].trim_start_matches(|c: char| c.is_ascii_digit());
    }
    out.push_str(rest);
    out
}
