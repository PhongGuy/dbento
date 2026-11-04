mod support;
use dbento_core::{ConnectError, Engine, ExecuteError, Limits, SessionOptions, Workspace, connect};
use support::{Cell, run, sqlite_fixture, text};

#[tokio::test]
async fn a_script_creates_then_inserts_then_selects() {
    let fx = sqlite_fixture("", "");
    let mut s = fx.session(SessionOptions::default()).await;
    assert_eq!(s.engine(), Engine::Sqlite);
    assert!(!s.is_read_only());
    assert!(!s.capabilities().schemas && s.capabilities().server_cancel);
    let sql = "CREATE TABLE t(a INTEGER, b TEXT); INSERT INTO t VALUES (1, 'x'), (2, NULL); SELECT a, b FROM t ORDER BY a -- done";
    let results: Vec<_> = run(&mut s, sql, Limits::default())
        .await
        .into_iter()
        .map(Result::unwrap)
        .collect();
    assert_eq!(results.len(), 3);
    assert_eq!(results[0].sql, "CREATE TABLE t(a INTEGER, b TEXT);");
    assert_eq!(results[1].sql, "INSERT INTO t VALUES (1, 'x'), (2, NULL);");
    assert_eq!(results[0].outcome.rows_affected, None);
    assert_eq!(results[1].outcome.rows_affected, Some(2));
    assert!(results[1].columns.is_empty());
    let select = &results[2];
    assert_eq!(
        select.columns,
        vec![
            ("a".to_owned(), "INTEGER".to_owned()),
            ("b".to_owned(), "TEXT".to_owned())
        ]
    );
    assert_eq!(
        select.rows,
        vec![vec![text("1"), text("x")], vec![text("2"), Cell::Null]]
    );
    assert_eq!(
        (
            select.outcome.rows,
            select.outcome.truncated,
            select.outcome.rows_affected
        ),
        (2, false, None)
    );
    s.close().await.unwrap();
}

#[tokio::test]
async fn values_render_as_text_or_bytes_and_never_as_null() {
    let fx = sqlite_fixture(
        "CREATE TABLE v(i INTEGER, d DECIMAL(10,2), x); INSERT INTO v VALUES (1, 2.5, 'z')",
        "",
    );
    let mut s = fx.session(SessionOptions::default()).await;
    let sql = "SELECT 42, -7, 0.1, 1.0, 1e300, 'héllo', '', NULL, x'00ff', CAST(x'ff' AS TEXT), 9223372036854775807";
    let ran = run(&mut s, sql, Limits::default()).await.remove(0).unwrap();
    assert_eq!(
        ran.rows,
        vec![vec![
            text("42"),
            text("-7"),
            text("0.1"),
            text("1.0"),
            text("1e300"),
            text("héllo"),
            text(""),
            Cell::Null,
            Cell::Bytes(vec![0x00, 0xff]),
            Cell::Bytes(vec![0xff]),
            text("9223372036854775807"),
        ]]
    );
    assert!(
        ran.columns.iter().all(|(_, ty)| ty.is_empty()),
        "expressions have no declared type"
    );
    let ran = run(&mut s, "SELECT i, d, x FROM v", Limits::default())
        .await
        .remove(0)
        .unwrap();
    assert_eq!(
        ran.columns,
        vec![
            ("i".into(), "INTEGER".into()),
            ("d".into(), "DECIMAL(10,2)".into()),
            ("x".into(), String::new())
        ]
    );
    assert_eq!(ran.rows, vec![vec![text("1"), text("2.5"), text("z")]]);
}

#[tokio::test]
async fn a_statement_that_fails_to_prepare_ends_the_script() {
    let fx = sqlite_fixture("", "");
    let mut s = fx.session(SessionOptions::default()).await;
    let mut script = s.script("SELECT 1; SELEC x; SELECT 2");
    let first = script.next().await.unwrap().unwrap();
    assert_eq!(first.sql(), "SELECT 1;");
    first.skip();
    let err = script.next().await.unwrap().err().expect("a syntax error");
    assert!(
        matches!(err, ExecuteError::Database { ref code, .. } if code == "1"),
        "{err:?}"
    );
    assert!(script.next().await.is_none());
}

#[tokio::test]
async fn a_missing_database_file_is_a_connect_error_and_is_not_created() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("dbento.toml"),
        "dbento = 1\n[connections.lite]\nengine = \"sqlite\"\npath = \"nope.db\"\n",
    )
    .unwrap();
    let ws = Workspace::open(dir.path()).unwrap();
    let err = connect(
        &ws,
        ws.connection("lite").unwrap(),
        None,
        SessionOptions::default(),
    )
    .await
    .err()
    .expect("connect must fail");
    assert!(matches!(err, ConnectError::NotFound { .. }), "{err:?}");
    assert!(!dir.path().join("nope.db").exists());
}

#[tokio::test]
async fn a_truncated_write_whose_commit_fails_reports_the_error() {
    let fx = sqlite_fixture(
        "CREATE TABLE p(a INTEGER PRIMARY KEY);
         CREATE TABLE t(a INTEGER REFERENCES p DEFERRABLE INITIALLY DEFERRED)",
        "",
    );
    let mut s = fx.session(SessionOptions::default()).await;
    let mut limits = Limits::default();
    limits.max_rows = Some(1);
    // No parent row exists, so the deferred check fails the implicit commit.
    let sql = "PRAGMA foreign_keys = ON; INSERT INTO t WITH RECURSIVE r(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM r WHERE x < 10) SELECT x FROM r RETURNING a";
    let results = run(&mut s, sql, limits).await;
    let closed = s.close().await;
    // Truncation resets inline on SQLite, so the statement itself reports the failed commit.
    match &results[..] {
        [Ok(_), Err(ExecuteError::Database { code, .. })] => assert_eq!(code, "787"),
        other => panic!(
            "{:?}",
            other
                .iter()
                .map(|r| r.as_ref().map(|r| &r.outcome))
                .collect::<Vec<_>>()
        ),
    }
    closed.unwrap();
    assert_eq!(fx.count("SELECT count(*) FROM t"), 0);
}

/// A 5000-row write whose commit the deferred key check fails, since no parent row exists.
const FAILING_WRITE: &str = "INSERT INTO t WITH RECURSIVE r(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM r WHERE x < 5000) SELECT x FROM r RETURNING a";

async fn deferred_key_fixture() -> (support::Fixture, dbento_core::Session) {
    let fx = sqlite_fixture(
        "CREATE TABLE p(a INTEGER PRIMARY KEY);
         CREATE TABLE t(a INTEGER REFERENCES p DEFERRABLE INITIALLY DEFERRED)",
        "",
    );
    let mut s = fx.session(SessionOptions::default()).await;
    run(&mut s, "PRAGMA foreign_keys = ON", Limits::default())
        .await
        .remove(0)
        .unwrap();
    (fx, s)
}

/// Starts `FAILING_WRITE` under `limits`, reads its first batch, and drops the stream.
async fn drop_after_first_batch(s: &mut dbento_core::Session, limits: Limits) {
    let mut script = s.script(FAILING_WRITE);
    let pending = script.next().await.unwrap().unwrap();
    let mut stream = pending.execute(limits).await.unwrap();
    assert!(stream.next_batch().await.unwrap().is_some());
}

fn assert_failed_commit(r: Result<(), ExecuteError>) {
    match r {
        Err(ExecuteError::Database { code, .. }) => assert_eq!(code, "787"),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn a_dropped_write_reports_its_failed_commit_on_the_next_settle() {
    let (fx, mut s) = deferred_key_fixture().await;
    drop_after_first_batch(&mut s, Limits::default()).await;
    assert_failed_commit(s.settle().await);
    // Reported once: the next settle owes nothing.
    s.settle().await.unwrap();
    s.close().await.unwrap();
    assert_eq!(fx.count("SELECT count(*) FROM t"), 0);
}

#[tokio::test]
async fn a_dropped_write_reports_its_failed_commit_on_close() {
    let (fx, mut s) = deferred_key_fixture().await;
    drop_after_first_batch(&mut s, Limits::default()).await;
    assert_failed_commit(s.close().await);
    assert_eq!(fx.count("SELECT count(*) FROM t"), 0);
}

#[tokio::test]
async fn a_dropped_parked_write_reports_its_failed_commit() {
    let (fx, mut s) = deferred_key_fixture().await;
    let mut limits = Limits::default();
    limits.max_rows = Some(10);
    limits.on_truncate = dbento_core::OnTruncate::Park;
    drop_after_first_batch(&mut s, limits).await;
    assert_failed_commit(s.settle().await);
    s.close().await.unwrap();
    assert_eq!(fx.count("SELECT count(*) FROM t"), 0);
}

// A session whose schema was loaded before another connection changed the table re-prepares
// on its first step; the columns it reports are the table's current ones, not the cached shape.
#[tokio::test]
async fn columns_follow_a_table_changed_by_another_connection() {
    let fx = sqlite_fixture(
        "CREATE TABLE t(a INTEGER, b TEXT); INSERT INTO t VALUES (1, 'x')",
        "",
    );
    let mut s = fx.session(SessionOptions::default()).await;
    let before = run(&mut s, "SELECT * FROM t", Limits::default())
        .await
        .remove(0)
        .unwrap();
    assert_eq!(before.columns.len(), 2);
    rusqlite::Connection::open(fx.db_path())
        .unwrap()
        .execute_batch("ALTER TABLE t RENAME COLUMN b TO c; ALTER TABLE t ADD COLUMN d TEXT")
        .unwrap();
    for sql in ["SELECT * FROM t", "SELECT * FROM t WHERE a > 5"] {
        let after = run(&mut s, sql, Limits::default()).await.remove(0).unwrap();
        let names: Vec<_> = after.columns.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, ["a", "c", "d"], "{sql}");
        assert!(after.rows.iter().all(|r| r.len() == 3), "{sql}");
    }
    s.close().await.unwrap();
}
