mod support;
use dbento_core::{Engine, ExecuteError, Limits, SessionOptions, Unguarded, Writes};
use std::time::Duration;
use support::{pg_fixture, run, text};

#[tokio::test]
async fn statements_run_in_order_with_rows_affected_from_command_complete() {
    let fx = pg_fixture("").await;
    let mut s = fx.session(SessionOptions::default()).await;
    assert_eq!(
        (s.engine(), s.capabilities().schemas),
        (Engine::Postgres, true)
    );
    let r = run(&mut s, "CREATE TABLE t(a int); INSERT INTO t SELECT generate_series(1, 3); UPDATE t SET a = a + 1 WHERE a > 1; INSERT INTO t VALUES (9) RETURNING a; DELETE FROM t WHERE false; CREATE TABLE u AS SELECT a FROM t", Limits::default()).await;
    let affected: Vec<_> = r
        .iter()
        .map(|x| x.as_ref().unwrap().outcome.rows_affected)
        .collect();
    // DDL counts no rows, so it reports none; CREATE TABLE AS keeps the count its tag carries.
    assert_eq!(
        affected,
        vec![None, Some(3), Some(2), None, Some(0), Some(4)]
    );
    assert_eq!(
        r[1].as_ref().unwrap().sql,
        "INSERT INTO t SELECT generate_series(1, 3);"
    );
    assert_eq!(
        r[3].as_ref().unwrap().columns,
        vec![("a".to_owned(), "int4".to_owned())]
    );
    assert_eq!(r[3].as_ref().unwrap().rows, vec![vec![text("9")]]);
}

#[tokio::test]
async fn a_script_prepares_lazily_and_stops_at_the_first_error() {
    let fx = pg_fixture("").await;
    let mut s = fx.session(SessionOptions::default()).await;
    let r = run(
        &mut s,
        "CREATE TABLE t(a int); INSERT INTO t VALUES (1); SELECT * FROM nope; SELECT 2",
        Limits::default(),
    )
    .await;
    assert_eq!(r.len(), 3);
    assert!(
        matches!(r[2], Err(ExecuteError::Database { ref code, .. }) if code == "42P01"),
        "{:?}",
        r[2].as_ref().err()
    );
    let r = run(
        &mut s,
        "CREATE TABLE ran(); SELECT 'oops",
        Limits::default(),
    )
    .await;
    assert!(
        matches!(r[0], Err(ExecuteError::Database { ref code, .. }) if code == "42601"),
        "{:?}",
        r[0].as_ref().err()
    );
    assert_eq!(
        fx.db
            .scalar("SELECT count(*) FROM pg_tables WHERE tablename = 'ran'")
            .await
            .as_deref(),
        Some("0")
    );
    let r = run(&mut s, "SELECT count(*) FROM t", Limits::default()).await;
    assert_eq!(r[0].as_ref().unwrap().rows, vec![vec![text("1")]]);
}

#[tokio::test]
async fn batches_hold_at_most_1024_rows_and_carry_type_oids() {
    let fx = pg_fixture("").await;
    let mut s = fx.session(SessionOptions::default()).await;
    let mut script = s.script("SELECT g::numeric FROM generate_series(1, 5000) g");
    let p = script.next().await.unwrap().unwrap();
    let mut stream = p.execute(Limits::default()).await.unwrap();
    assert_eq!(stream.columns()[0].pg_type_oid(), Some(1700));
    let mut total = 0;
    while let Some(b) = stream.next_batch().await.unwrap() {
        assert!(!b.is_empty() && b.len() <= 1024, "{}", b.len());
        total += b.len();
    }
    assert_eq!((total, stream.outcome().unwrap().rows), (5000, 5000));
}

#[tokio::test]
async fn pending_carries_the_classification() {
    let fx = pg_fixture("").await;
    let mut s = fx.session(SessionOptions::default()).await;
    run(&mut s, "CREATE TABLE t(a int)", Limits::default())
        .await
        .remove(0)
        .unwrap();
    {
        let mut script =
            s.script("WITH d AS (DELETE FROM t RETURNING *) SELECT * FROM d; -- note\nSELECT 1");
        let p = script.next().await.unwrap().unwrap();
        assert_eq!(
            (
                p.classification().writes,
                p.classification().unguarded.clone()
            ),
            (Writes::Yes, vec![Unguarded::DeleteWithoutWhere])
        );
        p.skip();
        let p = script.next().await.unwrap().unwrap();
        assert_eq!(
            (p.sql(), p.classification().writes),
            ("-- note\nSELECT 1", Writes::No)
        );
    }
}

#[tokio::test]
async fn cancel_stops_the_running_statement() {
    let fx = pg_fixture("").await;
    let mut s = fx.session(SessionOptions::default()).await;
    let cancel = s.cancel_handle();
    let t = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(300));
        cancel.cancel();
    });
    let r = run(&mut s, "SELECT pg_sleep(30)", Limits::default()).await;
    t.join().unwrap();
    assert!(
        matches!(r[0], Err(ExecuteError::Cancelled)),
        "{:?}",
        r[0].as_ref().err()
    );
    s.close().await.unwrap();
}

#[tokio::test]
async fn a_cancel_after_the_session_runtime_is_gone_does_nothing() {
    let fx = pg_fixture("").await;
    // The session lives on a runtime of its own, which is shut down before the cancel.
    let cancel = std::thread::scope(|scope| {
        scope
            .spawn(|| {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                let s = rt.block_on(fx.session(SessionOptions::default()));
                let cancel = s.cancel_handle();
                rt.block_on(s.close()).unwrap();
                cancel
            })
            .join()
            .unwrap()
    });
    cancel.cancel();
    cancel.cancel();
}
