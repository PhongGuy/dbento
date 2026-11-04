mod support;
use dbento_core::{ExecuteError, Limits, SessionOptions};
use support::{run, sqlite_fixture, text};

#[tokio::test]
async fn a_failed_stream_ends_and_the_session_stays_usable() {
    let fx = sqlite_fixture(
        "CREATE TABLE t(a INTEGER); INSERT INTO t VALUES (1), (2), (3)",
        "",
    );
    let mut s = fx.session(SessionOptions::default()).await;
    {
        let mut script =
            s.script("SELECT CASE WHEN a = 3 THEN abs(-9223372036854775807 - 1) ELSE a END FROM t");
        let p = script.next().await.unwrap().unwrap();
        let mut stream = p.execute(Limits::default()).await.unwrap();
        let err = stream
            .next_batch()
            .await
            .expect_err("integer overflow on row 3");
        assert!(
            matches!(err, ExecuteError::Database { ref message, .. } if message.contains("integer overflow")),
            "{err:?}"
        );
        assert!(stream.next_batch().await.unwrap().is_none());
        assert!(stream.outcome().is_none());
    }
    let r = run(&mut s, "SELECT count(*) FROM t", Limits::default()).await;
    assert_eq!(r[0].as_ref().unwrap().rows, vec![vec![text("3")]]);
}

#[tokio::test]
async fn rows_affected_counts_only_the_statements_own_rows() {
    let fx = sqlite_fixture(
        "CREATE TABLE t(a); CREATE TABLE log(m); CREATE TRIGGER tr AFTER INSERT ON t BEGIN INSERT INTO log VALUES ('x'); END;",
        "",
    );
    let mut s = fx.session(SessionOptions::default()).await;
    let r = run(
        &mut s,
        "INSERT INTO t VALUES (1), (2); CREATE TABLE u(a)",
        Limits::default(),
    )
    .await;
    assert_eq!(r[0].as_ref().unwrap().outcome.rows_affected, Some(2));
    assert_eq!(r[1].as_ref().unwrap().outcome.rows_affected, None);
}
