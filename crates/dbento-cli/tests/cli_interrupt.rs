#![cfg(unix)]
use crate::common::{LONG, count, sqlite_ws};
use std::io::{BufRead, BufReader, Read};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

fn spawn(ws: &tempfile::TempDir, sql: &str, tty: bool) -> Child {
    let mut cmd = Command::new(assert_cmd::cargo::cargo_bin("dbento"));
    cmd.current_dir(ws.path())
        .args(["query", "lite", sql, "--format", "json"])
        .env_remove("DBENTO_TEST_ASSUME_TTY")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if tty {
        cmd.env("DBENTO_TEST_ASSUME_TTY", "1");
    }
    cmd.spawn().unwrap()
}

fn interrupt(child: &Child) {
    assert!(
        Command::new("kill")
            .args(["-INT", &child.id().to_string()])
            .status()
            .unwrap()
            .success(),
        "kill delivers SIGINT"
    );
}

fn wait_at_most(child: &mut Child, limit: Duration) -> std::process::ExitStatus {
    let started = Instant::now();
    loop {
        if let Some(s) = child.try_wait().unwrap() {
            return s;
        }
        if started.elapsed() > limit {
            let _ = child.kill();
            panic!("dbento did not exit within {limit:?}");
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn the_first_ctrl_c_cancels_the_running_statement_and_stops_the_script() {
    let ws = sqlite_ws("CREATE TABLE t(a)");
    let mut child = spawn(&ws, &format!("{LONG}; insert into t values (1)"), false);
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut doc = String::new();
    stdout.read_line(&mut doc).unwrap(); // the header is written after the handler is installed
    assert!(doc.starts_with("{\"dbento\":1"), "{doc}");
    std::thread::sleep(Duration::from_millis(200));
    interrupt(&child);
    stdout.read_to_string(&mut doc).unwrap();
    assert_eq!(
        wait_at_most(&mut child, Duration::from_secs(5)).code(),
        Some(1)
    );
    let doc: serde_json::Value = serde_json::from_str(&doc).unwrap();
    let statements = doc["statements"].as_array().unwrap();
    assert_eq!(statements.len(), 1);
    assert_eq!(statements[0]["error"]["code"], "cancelled");
    assert_eq!(count(&ws, "select count(*) from t"), 0);
}

#[test]
fn ctrl_c_at_the_prompt_answers_no() {
    let ws = sqlite_ws("CREATE TABLE t(a); INSERT INTO t VALUES (1)");
    let mut child = spawn(&ws, "delete from t", true);
    let _stdin = child.stdin.take().unwrap(); // held open, so the prompt waits
    let mut stderr = child.stderr.take().unwrap();
    let mut seen = Vec::new();
    let mut byte = [0u8; 1];
    while !String::from_utf8_lossy(&seen).contains("[y/N]") {
        assert_eq!(
            stderr.read(&mut byte).unwrap(),
            1,
            "stderr closed before the prompt"
        );
        seen.push(byte[0]);
    }
    interrupt(&child);
    assert_eq!(
        wait_at_most(&mut child, Duration::from_secs(5)).code(),
        Some(2)
    );
    assert_eq!(count(&ws, "select count(*) from t"), 1);
}

#[test]
fn a_second_ctrl_c_exits_while_a_write_to_stdout_is_blocked() {
    let ws = sqlite_ws("CREATE TABLE t(a)");
    let endless = "WITH RECURSIVE r(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM r) SELECT x FROM r";
    let mut child = spawn(&ws, endless, false);
    // Read only the header, then hold the pipe open unread so it fills and the write blocks.
    let mut stdout = BufReader::new(child.stdout.take().unwrap());
    let mut header = String::new();
    stdout.read_line(&mut header).unwrap();
    assert!(header.starts_with("{\"dbento\":1"), "{header}");
    std::thread::sleep(Duration::from_millis(500));
    interrupt(&child);
    std::thread::sleep(Duration::from_millis(200));
    interrupt(&child);
    assert_eq!(
        wait_at_most(&mut child, Duration::from_secs(5)).code(),
        Some(1)
    );
    drop(stdout);
}
