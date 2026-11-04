mod support;
use std::time::Duration;

use dbento_core::{ExecuteError, Limits, OnTruncate, SessionOptions};
use support::{Cell, collect, pg_fixture, run, text};

#[tokio::test]
async fn values_are_the_servers_text_with_qualified_type_names() {
    let fx = pg_fixture("").await;
    fx.db
        .admin()
        .await
        .batch_execute("CREATE EXTENSION citext; CREATE TYPE mood AS ENUM ('ok')")
        .await
        .unwrap();
    let mut s = fx.session(SessionOptions::default()).await;
    let q = "SELECT 1.10::numeric AS n, '1 day 02:03:04'::interval AS i, '\\x0102'::bytea AS b, ARRAY[1,NULL]::int4[] AS a, 'Hi'::citext AS c, ARRAY['x']::citext[] AS ca, 'ok'::mood AS e, NULL::text AS z, ''::text AS empty, 'NaN'::float8 AS f";
    let ran = run(&mut s, q, Limits::default()).await.remove(0).unwrap();
    let want = [
        ("n", "numeric"),
        ("i", "interval"),
        ("b", "bytea"),
        ("a", "int4[]"),
        ("c", "public.citext"),
        ("ca", "public.citext[]"),
        ("e", "public.mood"),
        ("z", "text"),
        ("empty", "text"),
        ("f", "float8"),
    ];
    assert_eq!(
        ran.columns,
        want.map(|(n, t)| (n.to_owned(), t.to_owned())).to_vec()
    );
    assert_eq!(
        ran.rows,
        vec![vec![
            text("1.10"),
            text("1 day 02:03:04"),
            text("\\x0102"),
            text("{1,NULL}"),
            text("Hi"),
            text("{x}"),
            text("ok"),
            Cell::Null,
            text(""),
            text("NaN")
        ]]
    );
}

#[tokio::test]
async fn copy_with_the_client_and_encoding_changes_fail_without_running() {
    let fx = pg_fixture("").await;
    let mut s = fx.session(SessionOptions::default()).await;
    run(&mut s, "CREATE TABLE t(a int)", Limits::default())
        .await
        .remove(0)
        .unwrap();
    for sql in [
        "COPY t FROM STDIN",
        "COPY t TO STDOUT",
        "SET NAMES 'LATIN1'",
    ] {
        let r = run(&mut s, sql, Limits::default()).await;
        assert!(
            matches!(r[0], Err(ExecuteError::Unsupported { .. })),
            "{sql}: {:?}",
            r[0].as_ref().err()
        );
    }
    let r = run(&mut s, "SHOW client_encoding", Limits::default()).await;
    assert_eq!(r[0].as_ref().unwrap().rows, vec![vec![text("UTF8")]]);
}

#[tokio::test]
async fn text_that_is_not_utf8_is_unsupported_not_a_panic() {
    let fx = pg_fixture("").await;
    let mut s = fx.session(SessionOptions::default()).await;
    let r = run(
        &mut s,
        "SELECT set_config('client_encoding', 'LATIN1', false); SELECT chr(230)",
        Limits::default(),
    )
    .await;
    assert!(r[0].is_ok());
    assert!(
        matches!(r[1], Err(ExecuteError::Unsupported { .. })),
        "{:?}",
        r[1].as_ref().err()
    );
}

#[tokio::test]
async fn max_rows_truncates_only_when_more_rows_exist() {
    let fx = pg_fixture("").await;
    let mut s = fx.session(SessionOptions::default()).await;
    let mut limits = Limits::default();
    limits.max_rows = Some(2);
    let ran = run(&mut s, "SELECT g FROM generate_series(1, 5) g", limits)
        .await
        .remove(0)
        .unwrap();
    assert_eq!(ran.rows, vec![vec![text("1")], vec![text("2")]]);
    assert_eq!(
        (
            ran.outcome.rows,
            ran.outcome.truncated,
            ran.outcome.rows_affected
        ),
        (2, true, None)
    );
    limits.max_rows = Some(5);
    let ran = run(&mut s, "SELECT g FROM generate_series(1, 5) g", limits)
        .await
        .remove(0)
        .unwrap();
    assert_eq!((ran.outcome.rows, ran.outcome.truncated), (5, false));
    limits.max_rows = Some(0);
    let ran = run(&mut s, "SELECT 1", limits).await.remove(0).unwrap();
    assert_eq!((ran.rows.len(), ran.outcome.truncated), (0, true));
}

#[tokio::test]
async fn a_truncated_or_dropped_result_is_drained_and_the_session_continues() {
    let fx = pg_fixture("").await;
    let mut s = fx.session(SessionOptions::default()).await;
    let mut limits = Limits::default();
    limits.max_rows = Some(10);
    let r = run(
        &mut s,
        "SELECT g FROM generate_series(1, 200000) g; SELECT 'after'",
        limits,
    )
    .await;
    assert_eq!(r[0].as_ref().unwrap().outcome.rows, 10);
    assert_eq!(r[1].as_ref().unwrap().rows, vec![vec![text("after")]]);
    {
        let mut script = s.script("SELECT g FROM generate_series(1, 200000) g; SELECT 'next'");
        let p = script.next().await.unwrap().unwrap();
        let mut stream = p.execute(Limits::default()).await.unwrap();
        assert!(stream.next_batch().await.unwrap().is_some());
        drop(stream);
        let p = script.next().await.unwrap().unwrap();
        assert_eq!(
            collect(p, Limits::default()).await.unwrap().rows,
            vec![vec![text("next")]]
        );
    }
    let r = run(&mut s, "SELECT 'still'", Limits::default()).await;
    assert_eq!(r[0].as_ref().unwrap().rows, vec![vec![text("still")]]);
}

#[tokio::test]
async fn on_truncate_close_ends_the_session_and_the_server_query() {
    let fx = pg_fixture("").await;
    let mut s = fx.session(SessionOptions::default()).await;
    let mut limits = Limits::default();
    limits.max_rows = Some(3);
    limits.on_truncate = OnTruncate::Close;
    let r = run(
        &mut s,
        "SELECT g FROM generate_series(1, 5000000) g; SELECT 'after'",
        limits,
    )
    .await;
    let first = r[0].as_ref().unwrap();
    assert_eq!((first.outcome.rows, first.outcome.truncated), (3, true));
    assert!(
        matches!(r[1], Err(ExecuteError::SessionClosed)),
        "{:?}",
        r[1].as_ref().err()
    );
    let r = run(&mut s, "SELECT 1", Limits::default()).await;
    assert!(
        matches!(r[0], Err(ExecuteError::SessionClosed)),
        "{:?}",
        r[0].as_ref().err()
    );
    let ours = "SELECT count(*) FROM pg_stat_activity WHERE application_name = 'dbento' AND datname = current_database()";
    let mut left = fx.db.scalar(ours).await;
    for _ in 0..50 {
        if left.as_deref() == Some("0") {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
        left = fx.db.scalar(ours).await;
    }
    assert_eq!(
        left.as_deref(),
        Some("0"),
        "the server still runs the session's query"
    );
}

#[tokio::test]
async fn a_server_error_mid_result_under_close_keeps_the_session() {
    let fx = pg_fixture("").await;
    let mut s = fx.session(SessionOptions::default()).await;
    let mut limits = Limits::default();
    limits.max_rows = Some(100);
    limits.on_truncate = OnTruncate::Close;
    let bound = Duration::from_secs(30);
    // Rows 1 and 2 arrive before the division by zero, so the error ends a started result.
    let r = tokio::time::timeout(
        bound,
        run(
            &mut s,
            "SELECT 1 / (3 - g) FROM generate_series(1, 5) g",
            limits,
        ),
    )
    .await
    .expect("bounded");
    assert!(
        matches!(r[0], Err(ExecuteError::Database { .. })),
        "{:?}",
        r[0].as_ref().err()
    );
    let r = tokio::time::timeout(bound, run(&mut s, "SELECT 'alive'", limits))
        .await
        .expect("bounded");
    assert_eq!(r[0].as_ref().unwrap().rows, vec![vec![text("alive")]]);
}
