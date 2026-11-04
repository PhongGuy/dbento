//! The CLI end to end on Postgres, one container per test.
mod common;
use common::{dbento, write_tree};
use dbento_testkit::{PgDb, SECRET_VAR, pg_db};

fn workspace(db: &PgDb, files: &[(&str, &str)]) -> tempfile::TempDir {
    let toml = format!("dbento = 1\n{}", db.connection_toml("pg", ""));
    let mut all = vec![("dbento.toml", toml.as_str())];
    all.extend_from_slice(files);
    write_tree(&all)
}

fn dbento_pg(db: &PgDb, ws: &tempfile::TempDir, args: &[&str]) -> std::process::Output {
    dbento()
        .current_dir(ws.path())
        .env(SECRET_VAR, db.password())
        .args(args)
        .output()
        .unwrap()
}

async fn table_t(db: &PgDb) {
    db.admin()
        .await
        .batch_execute(
            "CREATE TABLE t(id int8, note text); INSERT INTO t VALUES (1, 'hello'), (2, NULL)",
        )
        .await
        .unwrap();
}

fn json(out: &std::process::Output) -> serde_json::Value {
    serde_json::from_slice(&out.stdout)
        .unwrap_or_else(|e| panic!("{e}: {}", String::from_utf8_lossy(&out.stdout)))
}

#[tokio::test]
async fn query_streams_postgres_json() {
    let db = pg_db().await;
    table_t(&db).await;
    let ws = workspace(&db, &[]);
    let out = dbento_pg(
        &db,
        &ws,
        &[
            "query",
            "pg",
            "select id, note from t order by id; insert into t values (3, 'x')",
            "--format",
            "json",
        ],
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let s = &json(&out)["statements"];
    assert_eq!(s[0]["sql"], "select id, note from t order by id;");
    assert_eq!(
        s[0]["columns"],
        serde_json::json!([{"name": "id", "type": "int8"}, {"name": "note", "type": "text"}])
    );
    assert_eq!(
        s[0]["rows"],
        serde_json::json!([["1", "hello"], ["2", null]])
    );
    assert_eq!(
        (s[1]["rows_affected"].clone(), s[1]["status"].clone()),
        (serde_json::json!(1), serde_json::json!("ok"))
    );
}

#[tokio::test]
async fn table_output_and_a_copy_refusal() {
    let db = pg_db().await;
    table_t(&db).await;
    let ws = workspace(&db, &[]);
    let out = dbento_pg(
        &db,
        &ws,
        &["query", "pg", "select id, note from t order by id"],
    );
    assert_eq!(out.status.code(), Some(0));
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "id | note\n---+------\n1  | hello\n2  |\n"
    );
    let out = dbento_pg(
        &db,
        &ws,
        &["query", "pg", "copy t from stdin", "--format", "json"],
    );
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(json(&out)["statements"][0]["error"]["code"], "unsupported");
}

#[tokio::test]
async fn run_refuses_an_unguarded_delete_without_a_tty() {
    let db = pg_db().await;
    table_t(&db).await;
    let ws = workspace(&db, &[("queries/pg/purge.sql", "DELETE FROM t;\n")]);
    let out = dbento_pg(
        &db,
        &ws,
        &["run", "queries/pg/purge.sql", "--format", "json"],
    );
    assert_eq!(out.status.code(), Some(2));
    let st = &json(&out)["statements"][0];
    assert_eq!(
        (st["status"].as_str(), st["reason"].as_str()),
        (Some("refused"), Some("delete_without_where"))
    );
    assert_eq!(
        db.scalar("SELECT count(*) FROM t").await.as_deref(),
        Some("2")
    );
}

#[tokio::test]
async fn a_wrong_secret_is_a_connect_error() {
    let db = pg_db().await;
    let ws = workspace(&db, &[]);
    let out = dbento()
        .current_dir(ws.path())
        .env(SECRET_VAR, "wrong-secret")
        .args(["query", "pg", "select 1"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(
        err.contains("password authentication failed") && !err.contains("wrong-secret"),
        "{err}"
    );
    assert!(out.stdout.is_empty());
}

#[test]
fn a_failed_connect_without_a_secret_prints_the_hint() {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let toml = format!(
        "dbento = 1\n[connections.pg]\nengine = \"postgres\"\nhost = \"127.0.0.1\"\nport = {port}\ndatabase = \"d\"\nuser = \"u\"\n"
    );
    let ws = write_tree(&[("dbento.toml", toml.as_str())]);
    let out = dbento()
        .current_dir(ws.path())
        .env("DBENTO_TEST_NO_KEYCHAIN", "1")
        .env_remove("DBENTO_SECRET_PG")
        .args(["query", "pg", "select 1"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(3));
    let err = String::from_utf8_lossy(&out.stderr);
    for part in [
        "hint:",
        "dbento secret set pg",
        "DBENTO_SECRET_PG",
        "disabled by DBENTO_TEST_NO_KEYCHAIN",
    ] {
        assert!(err.contains(part), "{part}: {err}");
    }
}

#[tokio::test]
async fn query_writes_postgres_csv() {
    let db = pg_db().await;
    table_t(&db).await;
    let ws = workspace(&db, &[]);
    let out = dbento_pg(
        &db,
        &ws,
        &[
            "query",
            "pg",
            "select id, note from t order by id",
            "--format",
            "csv",
        ],
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        "id,note\n1,hello\n2,\n"
    );
}

#[tokio::test]
async fn run_executes_a_saved_postgres_query() {
    let db = pg_db().await;
    table_t(&db).await;
    let ws = workspace(
        &db,
        &[(
            "queries/pg/add.sql",
            "INSERT INTO t VALUES (3, 'three');\nSELECT count(*) AS n FROM t;\n",
        )],
    );
    let out = dbento_pg(&db, &ws, &["run", "queries/pg/add.sql", "--format", "json"]);
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let s = &json(&out)["statements"];
    assert_eq!(s[0]["rows_affected"], 1);
    assert_eq!(s[1]["rows"], serde_json::json!([["3"]]));
    assert_eq!(
        db.scalar("SELECT note FROM t WHERE id = 3")
            .await
            .as_deref(),
        Some("three")
    );
}

#[cfg(unix)]
#[test]
fn ctrl_c_during_a_slow_connect_exits_1() {
    use std::time::{Duration, Instant};
    // 10.255.255.1 drops SYNs, so the connect hangs until the press.
    let toml = "dbento = 1\n[connections.pg]\nengine = \"postgres\"\nhost = \"10.255.255.1\"\n\
                database = \"d\"\nuser = \"u\"\n";
    let ws = write_tree(&[("dbento.toml", toml)]);
    let mut child = std::process::Command::new(assert_cmd::cargo::cargo_bin("dbento"))
        .current_dir(ws.path())
        .env("DBENTO_TEST_NO_KEYCHAIN", "1")
        .env("DBENTO_SECRET_PG", "p")
        .args(["query", "pg", "select 1"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(700));
    assert!(child.try_wait().unwrap().is_none(), "connect ended early");
    let pid = child.id().to_string();
    assert!(
        std::process::Command::new("kill")
            .args(["-INT", &pid])
            .status()
            .unwrap()
            .success()
    );
    let started = Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        if started.elapsed() > Duration::from_secs(10) {
            let _ = child.kill();
            panic!("dbento did not exit after Ctrl-C during connect");
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let mut err = String::new();
    std::io::Read::read_to_string(&mut child.stderr.take().unwrap(), &mut err).unwrap();
    assert_eq!(status.code(), Some(1), "{status:?}: {err}");
    assert!(err.contains("interrupted"), "{err}");
}

#[test]
fn a_zero_timeout_flag_is_a_connect_error() {
    let toml = "dbento = 1\n[connections.pg]\nengine = \"postgres\"\nhost = \"10.255.255.1\"\n\
                database = \"d\"\nuser = \"u\"\n";
    let ws = write_tree(&[("dbento.toml", toml)]);
    let mut child = std::process::Command::new(assert_cmd::cargo::cargo_bin("dbento"))
        .current_dir(ws.path())
        .env("DBENTO_TEST_NO_KEYCHAIN", "1")
        .env("DBENTO_SECRET_PG", "p")
        .args(["query", "pg", "select 1", "--timeout", "0s"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    // Bounded: a check that misses lets the connect hang on the blackholed host.
    let started = std::time::Instant::now();
    let status = loop {
        if let Some(s) = child.try_wait().unwrap() {
            break s;
        }
        if started.elapsed() > std::time::Duration::from_secs(10) {
            let _ = child.kill();
            panic!("dbento tried to connect with a zero timeout");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    };
    let mut err = String::new();
    std::io::Read::read_to_string(&mut child.stderr.take().unwrap(), &mut err).unwrap();
    assert_eq!(status.code(), Some(3), "{err}");
    assert!(err.contains("statement timeout"), "{err}");
}

#[tokio::test]
async fn a_read_only_postgres_query_refuses_writes_and_transaction_control() {
    let db = pg_db().await;
    table_t(&db).await;
    let ws = workspace(&db, &[]);
    let args = [
        "query",
        "pg",
        "select count(*) from t; delete from t where id = 1",
        "--read-only",
        "--format",
        "json",
    ];
    let out = dbento_pg(&db, &ws, &args);
    assert_eq!(
        out.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let s = &json(&out)["statements"];
    assert_eq!(s[0]["rows"], serde_json::json!([["2"]]));
    assert_eq!(s[1]["error"]["code"], "read_only");
    let out = dbento_pg(
        &db,
        &ws,
        &["query", "pg", "commit", "--read-only", "--format", "json"],
    );
    assert_eq!(out.status.code(), Some(1));
    assert_eq!(json(&out)["statements"][0]["error"]["code"], "unsupported");
    assert_eq!(
        db.scalar("select count(*) from t").await.as_deref(),
        Some("2")
    );
}

#[tokio::test]
async fn a_truncated_last_read_only_statement_ends_the_script_promptly() {
    let db = pg_db().await;
    let ws = workspace(&db, &[]);
    // About 30 s of slow rows: draining them would hit the cap and close the session.
    let slow = "SELECT repeat('x', 10000), pg_sleep(0.1) FROM generate_series(1, 300)";
    {
        let extra = "--read-only";
        let args = [
            "query",
            "pg",
            slow,
            "--max-rows",
            "1",
            "--format",
            "json",
            extra,
        ];
        let started = std::time::Instant::now();
        let out = dbento_pg(&db, &ws, &args);
        let waited = started.elapsed();
        assert_eq!(
            out.status.code(),
            Some(0),
            "{extra:?}: {}{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        let s = json(&out)["statements"].clone();
        assert_eq!(s.as_array().unwrap().len(), 1, "{extra:?}: {s}");
        assert_eq!(s[0]["truncated"], true, "{extra:?}: {s}");
        assert!(
            waited < std::time::Duration::from_secs(5),
            "{extra:?}: {waited:?}"
        );
    }
}

#[tokio::test]
async fn a_truncated_last_read_write_statement_past_the_cap_reports_the_session_closed() {
    let db = pg_db().await;
    let ws = workspace(&db, &[]);
    // It may write through a function, so it is drained, and 30 s of rows outlast the cap.
    let slow = "SELECT repeat('x', 10000), pg_sleep(0.1) FROM generate_series(1, 300)";
    let args = ["query", "pg", slow, "--max-rows", "1", "--format", "json"];
    let started = std::time::Instant::now();
    let out = dbento_pg(&db, &ws, &args);
    let waited = started.elapsed();
    assert_eq!(
        out.status.code(),
        Some(1),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let doc = json(&out);
    let s = doc["statements"].clone();
    assert_eq!(s.as_array().unwrap().len(), 1, "{doc}");
    assert_eq!(s[0]["truncated"], true, "{s}");
    // The stop is known only once the script ends, so it is the run's error, not a statement 2.
    // The code stays the contract's; the message says why the session closed.
    assert_eq!(doc["error"]["code"], "session_closed", "{doc}");
    assert_eq!(doc["error"]["message"], STOPPED_AT_CAP, "{doc}");
    assert!(
        waited >= std::time::Duration::from_millis(9500),
        "{waited:?}"
    );
}

/// What the CLI says when a cut-short statement that may write outlasts the drain cap.
const STOPPED_AT_CAP: &str = "the statement was still running when the row limit was hit, \
    so it was stopped and any writes it made were rolled back; \
    use --read-only for plain reads";

#[tokio::test]
async fn a_truncated_read_write_statement_past_the_cap_says_why_on_stderr() {
    let db = pg_db().await;
    let ws = workspace(&db, &[]);
    let slow = "SELECT repeat('x', 10000), pg_sleep(0.1) FROM generate_series(1, 300)";
    let out = dbento_pg(&db, &ws, &["query", "pg", slow, "--max-rows", "1"]);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(
        stderr.contains(&format!("error: after statement 1: {STOPPED_AT_CAP}")),
        "{stderr}"
    );
    assert!(!stderr.contains("statement 2"), "{stderr}");
    assert!(!stderr.contains("the session is closed"), "{stderr}");
}

#[tokio::test]
async fn a_truncated_read_write_statement_past_the_cap_mid_script_is_not_the_next_ones_error() {
    let db = pg_db().await;
    let ws = workspace(&db, &[]);
    let script = "SELECT repeat('x', 10000), pg_sleep(0.1) FROM generate_series(1, 300); SELECT 1";
    let args = ["query", "pg", script, "--max-rows", "1", "--format", "json"];
    let out = dbento_pg(&db, &ws, &args);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    // Draining the first statement hit the cap; the second never ran, so it reports nothing.
    let doc = json(&out);
    assert_eq!(doc["statements"].as_array().unwrap().len(), 1, "{doc}");
    assert_eq!(doc["error"]["code"], "session_closed", "{doc}");
    assert_eq!(doc["error"]["message"], STOPPED_AT_CAP, "{doc}");
    assert!(
        stderr.contains(&format!("error: after statement 1: {STOPPED_AT_CAP}")),
        "{stderr}"
    );
    assert!(!stderr.contains("statement 2"), "{stderr}");
}

#[tokio::test]
async fn truncate_and_drop_are_refused_without_a_terminal_and_run_with_yes() {
    let db = pg_db().await;
    table_t(&db).await;
    let ws = workspace(&db, &[]);
    for (sql, reason) in [("truncate t", "truncate"), ("drop table t", "drop")] {
        let out = dbento_pg(&db, &ws, &["query", "pg", sql, "--format", "json"]);
        assert_eq!(out.status.code(), Some(2), "{sql}");
        assert_eq!(json(&out)["statements"][0]["reason"], reason, "{sql}");
    }
    let out = dbento_pg(
        &db,
        &ws,
        &[
            "query",
            "pg",
            "truncate t; drop table t",
            "--format",
            "json",
            "--yes",
        ],
    );
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let gone: bool = db
        .admin()
        .await
        .query_one("SELECT to_regclass('public.t') IS NULL", &[])
        .await
        .unwrap()
        .get(0);
    assert!(gone);
}

#[tokio::test]
async fn a_truncated_write_whose_commit_fails_exits_non_zero() {
    let db = pg_db().await;
    db.admin()
        .await
        .batch_execute(
            "CREATE TABLE p(a int PRIMARY KEY);
             CREATE TABLE c(a int REFERENCES p DEFERRABLE INITIALLY DEFERRED)",
        )
        .await
        .unwrap();
    let ws = workspace(&db, &[]);
    // No parent row exists, so the commit after the cut-short result fails its deferred check.
    let out = dbento_pg(
        &db,
        &ws,
        &[
            "query",
            "pg",
            "INSERT INTO c SELECT generate_series(1, 100) RETURNING a",
            "--max-rows",
            "1",
            "--format",
            "json",
        ],
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    assert!(stderr.contains("23503"), "{stderr}");
    // The statement's own object was already streamed as ok, so the run carries the error.
    let doc = json(&out);
    assert_eq!(doc["statements"][0]["truncated"], true);
    assert_eq!(doc["error"]["code"], "23503", "{doc}");
    assert!(doc["error"]["message"].is_string(), "{doc}");
}

#[tokio::test]
async fn a_truncated_write_whose_commit_fails_mid_script_marks_the_json_run_failed() {
    let db = pg_db().await;
    db.admin()
        .await
        .batch_execute(
            "CREATE TABLE p(a int PRIMARY KEY);
             CREATE TABLE c(a int REFERENCES p DEFERRABLE INITIALLY DEFERRED)",
        )
        .await
        .unwrap();
    let ws = workspace(
        &db,
        &[(
            "queries/w.sql",
            "INSERT INTO c SELECT generate_series(1, 100) RETURNING a;\nSELECT 1;\n",
        )],
    );
    // The failed commit stays unreported past the next statement, until the session closes.
    let out = dbento_pg(
        &db,
        &ws,
        &[
            "run",
            "queries/w.sql",
            "--conn",
            "pg",
            "--max-rows",
            "1",
            "--format",
            "json",
        ],
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert_eq!(out.status.code(), Some(1), "{stderr}");
    let doc = json(&out);
    assert_eq!(doc["error"]["code"], "23503", "{doc}");
}

#[tokio::test]
async fn a_clean_json_run_has_no_run_error() {
    let db = pg_db().await;
    table_t(&db).await;
    let ws = workspace(&db, &[]);
    let out = dbento_pg(
        &db,
        &ws,
        &[
            "query",
            "pg",
            "insert into t values (3, 'x')",
            "--max-rows",
            "1",
            "--format",
            "json",
        ],
    );
    assert_eq!(out.status.code(), Some(0));
    assert!(json(&out).get("error").is_none());
}
