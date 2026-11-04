//! `dbento query` and `dbento run` on MySQL and MariaDB: the statement timeout and Ctrl-C stop
//! the statement on the server, and a drain past the cap is worded for MySQL.
mod common;
use std::time::{Duration, Instant};

use common::{dbento, write_tree};
use dbento_testkit::{MY_SECRET_VAR, MyDb, NUMS_SQL, my_matrix};

/// What the CLI says when a cut-short MySQL statement that may write outlasts the drain cap.
const MYSQL_STOPPED_AT_CAP: &str = "the statement was still running when the row limit was hit, \
    so it was stopped; a transactional write was rolled back, but a non-transactional one may \
    have kept part of its changes; use --read-only for plain reads";

/// A workspace whose connection `my` is `db`, plus `files` (relative path, contents).
fn workspace(db: &MyDb, files: &[(&str, &str)]) -> tempfile::TempDir {
    let toml = format!("dbento = 1\n{}", db.connection_toml("my", ""));
    let mut all = vec![("dbento.toml", toml.as_str())];
    all.extend_from_slice(files);
    write_tree(&all)
}

/// Exit code, stdout and stderr of `dbento` with `args`, without a terminal.
fn cli(db: &MyDb, ws: &tempfile::TempDir, args: &[&str]) -> (i32, String, String) {
    let out = dbento()
        .current_dir(ws.path())
        .env(MY_SECRET_VAR, db.password())
        .args(args)
        .output()
        .unwrap();
    (
        out.status.code().unwrap(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn doc(stdout: &str) -> serde_json::Value {
    serde_json::from_str(stdout).unwrap_or_else(|e| panic!("{e}: {stdout}"))
}

/// Statements on `db`'s database whose text is exactly `sql`, counted from the process list.
fn running(db: &MyDb, sql: &str) -> String {
    format!(
        "SELECT COUNT(*) FROM information_schema.PROCESSLIST WHERE INFO = '{sql}' AND DB = '{}'",
        db.database()
    )
}

/// Polls `sql` on `db` until it answers `want`, for up to 5 s.
async fn until(db: &MyDb, sql: &str, want: &str) -> bool {
    until_within(db, sql, want, 50).await
}

/// Polls `sql` on `db` until it answers `want`, at most `tries` times 100 ms apart.
async fn until_within(db: &MyDb, sql: &str, want: &str, tries: u32) -> bool {
    for _ in 0..tries {
        if db.scalar(sql).await.as_deref() == Some(want) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

#[tokio::test]
async fn the_statement_timeout_stops_a_mysql_query_and_a_saved_run() {
    for (label, db) in my_matrix().await {
        let ws = workspace(&db, &[("queries/my/slow.sql", "SELECT SLEEP(5)")]);
        for base in [
            vec!["query", "my", "SELECT SLEEP(5)"],
            vec!["run", "queries/my/slow.sql"],
        ] {
            let mut args = base.clone();
            args.extend(["--timeout", "500ms", "--format", "json"]);
            let started = Instant::now();
            let (code, out, err) = cli(&db, &ws, &args);
            let took = started.elapsed();
            assert_eq!(code, 1, "{label} {base:?}: {err}");
            assert_eq!(
                doc(&out)["statements"][0]["error"]["code"],
                "timeout",
                "{label} {base:?}: {out}"
            );
            assert!(took < Duration::from_secs(4), "{label} {base:?}: {took:?}");
            // The KILL lands asynchronously, so poll for about 1 s: still well inside the 5 s
            // sleep, so a sleep listed throughout was not stopped.
            assert!(
                until_within(&db, &running(&db, "SELECT SLEEP(5)"), "0", 10).await,
                "{label} {base:?}: the sleep runs on after the timeout"
            );
        }
    }
}

#[cfg(unix)]
#[tokio::test]
async fn the_first_ctrl_c_stops_a_mysql_statement_on_the_server() {
    use std::io::Read;
    use std::process::{Command, Stdio};
    for (label, db) in my_matrix().await {
        let ws = workspace(&db, &[]);
        let mut child = Command::new(assert_cmd::cargo::cargo_bin("dbento"))
            .current_dir(ws.path())
            .env(MY_SECRET_VAR, db.password())
            .env_remove("DBENTO_TEST_ASSUME_TTY")
            .args(["query", "my", "SELECT SLEEP(60)", "--format", "json"])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let sleeping = running(&db, "SELECT SLEEP(60)");
        assert!(
            until(&db, &sleeping, "1").await,
            "{label}: the sleep never started"
        );
        assert!(
            Command::new("kill")
                .args(["-INT", &child.id().to_string()])
                .status()
                .unwrap()
                .success()
        );
        let pressed = Instant::now();
        let status = loop {
            if let Some(s) = child.try_wait().unwrap() {
                break s;
            }
            if pressed.elapsed() > Duration::from_secs(10) {
                let _ = child.kill();
                panic!("{label}: dbento did not exit within 10 s of Ctrl-C");
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        assert_eq!(status.code(), Some(1), "{label}");
        let mut out = String::new();
        child
            .stdout
            .take()
            .unwrap()
            .read_to_string(&mut out)
            .unwrap();
        let doc = doc(&out);
        let statements = doc["statements"].as_array().unwrap();
        assert_eq!(statements.len(), 1, "{label}: {out}");
        assert_eq!(
            statements[0]["error"]["code"], "cancelled",
            "{label}: {out}"
        );
        assert!(
            until(&db, &sleeping, "0").await,
            "{label}: the sleep runs on after Ctrl-C"
        );
    }
}

#[tokio::test]
async fn a_mysql_drain_past_the_cap_says_a_non_transactional_write_may_stay() {
    for (label, db) in my_matrix().await {
        db.scalar(NUMS_SQL).await;
        let ws = workspace(&db, &[]);
        // 1e9 rows: the read-write drain after the first row outlasts the 10 s cap.
        let (code, out, err) = cli(
            &db,
            &ws,
            &[
                "query",
                "my",
                "SELECT a.n FROM nums a, nums b, nums c",
                "--max-rows",
                "1",
                "--format",
                "json",
            ],
        );
        assert_eq!(code, 1, "{label}: {err}");
        let doc = doc(&out);
        assert_eq!(doc["statements"][0]["truncated"], true, "{label}: {doc}");
        assert_eq!(doc["error"]["code"], "session_closed", "{label}: {doc}");
        assert_eq!(
            doc["error"]["message"], MYSQL_STOPPED_AT_CAP,
            "{label}: {doc}"
        );
        assert!(err.contains(MYSQL_STOPPED_AT_CAP), "{label}: {err}");
    }
}

#[tokio::test]
async fn a_call_counts_the_result_sets_it_does_not_show() {
    for (label, db) in my_matrix().await {
        db.scalar(
            "CREATE PROCEDURE three() BEGIN SELECT 1 AS a; SELECT 2 AS b; SELECT 3 AS c; END",
        )
        .await;
        let ws = workspace(&db, &[]);
        let (code, out, err) = cli(
            &db,
            &ws,
            &[
                "query",
                "my",
                "CALL three(); SELECT 4 AS d",
                "--format",
                "json",
            ],
        );
        assert_eq!(code, 0, "{label}: {err}");
        let s = doc(&out)["statements"].clone();
        assert_eq!(s[0]["rows"], serde_json::json!([["1"]]), "{label}: {s}");
        assert_eq!(s[0]["extra_results"], 2, "{label}: {s}");
        assert_eq!(s[1]["rows"], serde_json::json!([["4"]]), "{label}: {s}");
        assert!(s[1].get("extra_results").is_none(), "{label}: {s}");
        let (code, out, err) = cli(&db, &ws, &["query", "my", "CALL three()"]);
        assert_eq!(code, 0, "{label}: {err}");
        assert!(out.contains('1'), "{label}: {out}");
        assert!(
            err.contains("(1 row, 2 more result sets not shown, "),
            "{label}: {err}"
        );
        let (code, _, err) = cli(
            &db,
            &ws,
            &["query", "my", "CALL three()", "--format", "csv"],
        );
        assert_eq!(code, 0, "{label}: {err}");
        assert!(
            err.contains("statement 1: 2 more result sets not shown"),
            "{label}: {err}"
        );
    }
}

#[tokio::test]
async fn a_mysql_error_reports_its_number_and_sqlstate() {
    for (label, db) in my_matrix().await {
        let ws = workspace(&db, &[]);
        let (code, out, err) = cli(
            &db,
            &ws,
            &["query", "my", "SELECT * FROM nope", "--format", "json"],
        );
        assert_eq!(code, 1, "{label}: {err}");
        let e = &doc(&out)["statements"][0]["error"];
        assert_eq!(
            (
                e["code"].clone(),
                e["sqlstate"].clone(),
                e["error_number"].clone()
            ),
            (
                serde_json::json!("1146"),
                serde_json::json!("42S02"),
                serde_json::json!("1146")
            ),
            "{label}: {e}"
        );
        assert!(
            e["message"].as_str().unwrap().contains("nope"),
            "{label}: {e}"
        );
        let (code, _, err) = cli(&db, &ws, &["query", "my", "SELECT * FROM nope"]);
        assert_eq!(code, 1, "{label}: {err}");
        assert!(
            err.contains("error: statement 1: ") && err.contains("(1146, SQLSTATE 42S02)"),
            "{label}: {err}"
        );
    }
}
