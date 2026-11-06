#![cfg(feature = "duckdb")]
mod support;
use dbento_core::{ConnectError, Engine, ExecuteError, Limits, SessionOptions};
use support::{Cell, duckdb_fixture, run, text};

#[tokio::test]
async fn a_script_creates_inserts_and_selects() {
    let fx = duckdb_fixture("", "");
    let mut s = fx.session(SessionOptions::default()).await;
    assert_eq!(s.engine(), Engine::Duckdb);
    assert!(!s.is_read_only() && s.server().is_none());
    let sql = "CREATE TABLE t(a INTEGER, b VARCHAR); INSERT INTO t VALUES (1, 'x'), (2, NULL); SELECT a, b FROM t ORDER BY a";
    let results: Vec<_> = run(&mut s, sql, Limits::default())
        .await
        .into_iter()
        .map(Result::unwrap)
        .collect();
    assert_eq!(results.len(), 3);
    assert_eq!(results[0].sql, "CREATE TABLE t(a INTEGER, b VARCHAR);");
    assert!(results[0].columns.is_empty() && results[0].outcome.rows_affected.is_none());
    assert!(results[1].columns.is_empty());
    assert_eq!(results[1].outcome.rows_affected, Some(2));
    assert_eq!(
        results[2].columns,
        [
            ("a".to_owned(), "INTEGER".to_owned()),
            ("b".to_owned(), "VARCHAR".to_owned())
        ]
    );
    assert_eq!(
        results[2].rows,
        [vec![text("1"), text("x")], vec![text("2"), Cell::Null]]
    );
    assert_eq!(
        (results[2].outcome.rows, results[2].outcome.truncated),
        (2, false)
    );
    s.close().await.unwrap();
}

#[tokio::test]
async fn status_results_count_rows_and_returning_streams_them() {
    let fx = duckdb_fixture(
        "CREATE TABLE t(a INTEGER); INSERT INTO t VALUES (1), (2), (3)",
        "",
    );
    let mut s = fx.session(SessionOptions::default()).await;
    let sql = "UPDATE t SET a = a * 10 WHERE a > 1; DELETE FROM t WHERE a = 1; INSERT INTO t VALUES (7) RETURNING a";
    let r: Vec<_> = run(&mut s, sql, Limits::default())
        .await
        .into_iter()
        .map(Result::unwrap)
        .collect();
    assert_eq!(r[0].outcome.rows_affected, Some(2));
    assert_eq!(r[1].outcome.rows_affected, Some(1));
    assert_eq!(r[2].columns, [("a".to_owned(), "INTEGER".to_owned())]);
    assert_eq!(r[2].rows, [vec![text("7")]]);
    assert_eq!(r[2].outcome.rows_affected, None);
    assert!(!s.settings_changed());
    let r = run(&mut s, "SET threads = 2", Limits::default())
        .await
        .remove(0)
        .unwrap();
    assert!(r.columns.is_empty() && r.outcome.rows_affected.is_none());
    assert!(s.settings_changed());
}

#[tokio::test]
async fn a_failing_statement_ends_the_script_with_duckdbs_text() {
    let fx = duckdb_fixture("", "");
    let mut s = fx.session(SessionOptions::default()).await;
    let r = run(
        &mut s,
        "SELECT 1; SELECT * FROM nope; SELECT 3",
        Limits::default(),
    )
    .await;
    assert_eq!(r.len(), 2);
    match &r[1] {
        Err(ExecuteError::Database { code, message, .. }) => {
            assert_eq!(code, "Catalog");
            assert!(
                message.contains("nope") && !message.starts_with("Catalog Error"),
                "{message}"
            );
        }
        other => panic!("{:?}", other.as_ref().err()),
    }
    assert_eq!(
        run(&mut s, "SELECT 4", Limits::default())
            .await
            .remove(0)
            .unwrap()
            .rows,
        [vec![text("4")]]
    );
}

#[tokio::test]
async fn statements_run_one_at_a_time_so_a_skipped_one_never_runs() {
    let fx = duckdb_fixture("CREATE TABLE t(a INTEGER)", "");
    let mut s = fx.session(SessionOptions::default()).await;
    {
        let mut script = s.script("INSERT INTO t VALUES (1); SELECT count(*) FROM t");
        let mut first = script.next().await.unwrap().unwrap();
        assert_eq!(first.sql(), "INSERT INTO t VALUES (1);");
        assert!(!first.is_last().await);
        first.skip();
        let mut second = script.next().await.unwrap().unwrap();
        assert!(second.is_last().await);
        let ran = support::collect(second, Limits::default()).await.unwrap();
        assert_eq!(ran.rows, [vec![text("0")]]);
        assert!(script.next().await.is_none());
    }
    s.close().await.unwrap();
}

#[tokio::test]
async fn an_undecodable_column_fails_before_any_row_and_names_the_cast() {
    let fx = duckdb_fixture("", "");
    let mut s = fx.session(SessionOptions::default()).await;
    match run(
        &mut s,
        "SELECT 1 AS n, TIMETZ '12:00:00+02' AS l",
        Limits::default(),
    )
    .await
    .remove(0)
    {
        Err(ExecuteError::Unsupported { reason }) => assert_eq!(
            reason,
            "column `l` has type `TIME WITH TIME ZONE`, which Dbento cannot display yet; select `CAST(l AS VARCHAR)` instead"
        ),
        other => panic!("{:?}", other.err()),
    }
    assert_eq!(
        run(&mut s, "SELECT 42", Limits::default())
            .await
            .remove(0)
            .unwrap()
            .rows,
        [vec![text("42")]]
    );
}

#[tokio::test]
async fn a_write_refused_for_display_runs_to_its_end_and_says_so() {
    let fx = duckdb_fixture("CREATE TABLE t(a BIGINT)", "");
    let mut s = fx.session(SessionOptions::default()).await;
    let sql = "INSERT INTO t SELECT range FROM range(10000) RETURNING a, TIMETZ '12:00:00+02' AS l";
    match run(&mut s, sql, Limits::default()).await.remove(0) {
        Err(ExecuteError::Unsupported { reason }) => {
            assert!(
                reason.contains("the statement's changes were applied"),
                "{reason}"
            )
        }
        other => panic!("{:?}", other.err()),
    }
    s.close().await.unwrap();
    assert_eq!(fx.scalar("SELECT count(*) FROM t").await, text("10000"));
}

#[tokio::test]
async fn a_returning_column_named_count_is_data() {
    let fx = duckdb_fixture("CREATE TABLE t(x BIGINT)", "");
    let mut s = fx.session(SessionOptions::default()).await;
    let r = run(
        &mut s,
        "INSERT INTO t VALUES (5) RETURNING x AS \"Count\"",
        Limits::default(),
    )
    .await
    .remove(0)
    .unwrap();
    assert_eq!(r.columns, [("Count".to_owned(), "BIGINT".to_owned())]);
    assert_eq!(r.rows, [vec![text("5")]]);
    assert_eq!(r.outcome.rows_affected, None);
    s.close().await.unwrap();
}

#[tokio::test]
async fn a_refusal_claims_applied_changes_only_for_a_parsed_write() {
    let fx = duckdb_fixture("", "");
    let mut s = fx.session(SessionOptions::default()).await;
    let may = "; the statement may have changed data";
    for (sql, kept) in [
        ("CALL query('SELECT TIMETZ ''12:00:00+02'' AS l')", may),
        ("(FROM range(1) SELECT TIMETZ '12:00:00+02' AS l)", may),
        ("SELECT TIMETZ '12:00:00+02' AS l", ""),
    ] {
        match run(&mut s, sql, Limits::default()).await.remove(0) {
            Err(ExecuteError::Unsupported { reason }) => {
                assert!(
                    reason.ends_with(&format!("instead{kept}")),
                    "{sql}: {reason}"
                );
                assert!(!reason.contains("were applied"), "{sql}: {reason}");
            }
            other => panic!("{sql}: {:?}", other.err()),
        }
    }
    s.close().await.unwrap();
}

#[tokio::test]
async fn the_cast_hint_quotes_the_column_name() {
    let fx = duckdb_fixture("", "");
    let mut s = fx.session(SessionOptions::default()).await;
    match run(
        &mut s,
        "SELECT TIMETZ '12:00:00+02' AS \"My Col\"",
        Limits::default(),
    )
    .await
    .remove(0)
    {
        Err(ExecuteError::Unsupported { reason }) => {
            assert!(
                reason.contains("select `CAST(\"My Col\" AS VARCHAR)`"),
                "{reason}"
            )
        }
        other => panic!("{:?}", other.err()),
    }
    s.close().await.unwrap();
}

#[tokio::test]
async fn a_missing_file_is_not_found_and_never_created() {
    let fx = duckdb_fixture(
        "",
        "[connections.gone]\nengine = \"duckdb\"\npath = \"data/gone.duckdb\"\n",
    );
    match fx.try_session_of("gone", SessionOptions::default()).await {
        Err(ConnectError::NotFound { .. }) => {}
        other => panic!("{:?}", other.err()),
    }
    assert!(!fx.dir.path().join("data/gone.duckdb").exists());
}

#[tokio::test]
async fn close_releases_the_file_for_the_other_mode() {
    let fx = duckdb_fixture("CREATE TABLE t(a INTEGER)", "");
    let s = fx.session(SessionOptions::default()).await;
    s.close().await.unwrap();
    let mut ro = SessionOptions::default();
    ro.force_read_only = true;
    let r = fx.session(ro).await;
    assert!(r.is_read_only());
    r.close().await.unwrap();
}

#[tokio::test]
async fn a_variant_column_is_refused_by_its_position() {
    let fx = duckdb_fixture("", "");
    let mut s = fx.session(SessionOptions::default()).await;
    for sql in [
        "SELECT 1 AS a, 1::VARIANT AS v",
        "SELECT 1 AS a, {'v': 1::VARIANT} AS s",
    ] {
        match run(&mut s, sql, Limits::default()).await.remove(0) {
            Err(ExecuteError::Unsupported { reason }) => assert_eq!(
                reason,
                "column 2 has type `VARIANT`, which Dbento cannot display yet; select it with `CAST(… AS VARCHAR)` instead",
                "{sql}"
            ),
            other => panic!("{sql}: {:?}", other.err()),
        }
    }
    assert_eq!(
        run(&mut s, "SELECT 42", Limits::default())
            .await
            .remove(0)
            .unwrap()
            .rows,
        [vec![text("42")]]
    );
    s.close().await.unwrap();
}

#[tokio::test]
async fn a_use_of_the_attached_database_marks_the_session() {
    let fx = duckdb_fixture("", "");
    let mut s = fx.session(SessionOptions::default()).await;
    let db = run(&mut s, "SELECT current_database() AS d", Limits::default())
        .await
        .remove(0)
        .unwrap()
        .rows
        .remove(0)
        .remove(0);
    let Cell::Text(db) = db else {
        panic!("current_database() gave {db:?}")
    };
    assert!(!s.settings_changed());
    run(&mut s, &format!("USE \"{db}\""), Limits::default())
        .await
        .remove(0)
        .unwrap();
    assert!(s.settings_changed());
}
