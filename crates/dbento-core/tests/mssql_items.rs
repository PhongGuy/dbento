//! A batch's result sets and row counts as script results, errors in order, GO counts and is_last.

use crate::support::{Cell, collect, ms_off, run, text};
use dbento_core::{ExecuteError, Limits, SessionOptions, Writes};

#[tokio::test]
async fn each_result_set_and_row_count_is_a_result_with_the_batch_text() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_off().await;
    let mut s = fx.session(SessionOptions::default()).await;
    let script = "CREATE TABLE dbo.g (n int)\nGO\nINSERT dbo.g VALUES (1)\nGO 3\n\
                  UPDATE dbo.g SET n = 2; SELECT n FROM dbo.g WHERE n = 2\nGO\n\
                  EXEC sp_executesql N'UPDATE dbo.g SET n = n'\nGO\nDECLARE @x int = 1";
    let ran: Vec<_> = run(&mut s, script, Limits::default())
        .await
        .into_iter()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        ran.len(),
        8,
        "{:?}",
        ran.iter().map(|r| &r.sql).collect::<Vec<_>>()
    );
    assert_eq!(
        (ran[0].sql.as_str(), ran[0].outcome.rows_affected),
        ("CREATE TABLE dbo.g (n int)", None)
    );
    for r in &ran[1..4] {
        assert_eq!(
            (r.sql.as_str(), r.outcome.rows_affected),
            ("INSERT dbo.g VALUES (1)", Some(1))
        );
        assert!(r.columns.is_empty());
    }
    let batch = "UPDATE dbo.g SET n = 2; SELECT n FROM dbo.g WHERE n = 2";
    assert_eq!(
        (ran[4].sql.as_str(), ran[4].outcome.rows_affected),
        (batch, Some(3))
    );
    assert_eq!(ran[5].sql, batch);
    assert_eq!(ran[5].rows, vec![vec![text("2")]; 3]);
    assert_eq!(
        (ran[5].outcome.rows_affected, ran[5].outcome.extra_results),
        (None, 0)
    );
    assert_eq!(
        ran[6].outcome.rows_affected,
        Some(3),
        "DONEINPROC counts, DONEPROC opens nothing"
    );
    assert!(ran[7].columns.is_empty() && ran[7].rows.is_empty());
    assert_eq!(ran[7].outcome.rows_affected, None);
}

#[tokio::test]
async fn a_statement_error_is_its_items_and_ends_the_script_after_the_server_ran_the_rest() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_off().await;
    let mut s = fx.session(SessionOptions::default()).await;
    run(&mut s, "CREATE TABLE dbo.e (n int)", Limits::default())
        .await
        .remove(0)
        .unwrap();
    {
        let mut script = s.script(
            "SELECT 1 AS a; SELECT 1/0 AS boom; INSERT dbo.e VALUES (9)\nGO\nSELECT 5 AS never",
        );
        let first = collect(script.next().await.unwrap().unwrap(), Limits::default())
            .await
            .unwrap();
        assert_eq!(first.rows, vec![vec![text("1")]]);
        match collect(script.next().await.unwrap().unwrap(), Limits::default()).await {
            Err(ExecuteError::Database { code, message, .. }) => {
                assert_eq!(code, "8134");
                assert!(message.contains("Divide by zero"), "{message}");
            }
            other => panic!("{:?}", other.map(|r| r.rows)),
        }
        assert!(script.next().await.is_none(), "the error ends the script");
    }
    let count = run(
        &mut s,
        "SELECT COUNT(*) AS c FROM dbo.e WHERE n = 9",
        Limits::default(),
    )
    .await;
    assert_eq!(
        count[0].as_ref().unwrap().rows,
        vec![vec![text("1")]],
        "the drained INSERT ran"
    );
    match run(&mut s, "SELECT * FROM dbo.nope", Limits::default())
        .await
        .remove(0)
    {
        Err(ExecuteError::Database { code, message, .. }) => {
            assert_eq!(code, "208");
            assert!(message.contains("dbo.nope"), "{message}");
        }
        other => panic!("{:?}", other.map(|r| r.rows)),
    }
    let repeated = run(&mut s, "INSERT dbo.e VALUES (1/0)\nGO 3", Limits::default()).await;
    assert_eq!(repeated.len(), 1, "the failed first send ends the script");
    assert!(matches!(&repeated[0], Err(ExecuteError::Database { code, .. }) if code == "8134"));
    let later = run(
        &mut s,
        "SELECT 1 AS a; SELECT CAST(0x01 AS varbinary(4)) AS b",
        Limits::default(),
    )
    .await;
    assert_eq!(later[0].as_ref().unwrap().rows, vec![vec![text("1")]]);
    assert_eq!(
        later[1].as_ref().unwrap().rows,
        vec![vec![Cell::Bytes(vec![1])]]
    );
}

#[tokio::test]
async fn only_a_last_single_query_batch_without_a_count_is_last() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_off().await;
    let mut s = fx.session(SessionOptions::default()).await;
    for (script, want) in [
        ("SELECT 1 AS a\nGO\nSELECT 2 AS b", vec![false, true]),
        ("SELECT 1 AS a; SELECT 2 AS b", vec![false, false]),
        ("SELECT 1 AS a\nGO 2", vec![false, false]),
        ("DECLARE @x int = 1", vec![false]),
    ] {
        let mut got = Vec::new();
        let mut sc = s.script(script);
        while let Some(p) = sc.next().await {
            let mut p = p.unwrap();
            got.push(p.is_last().await);
            collect(p, Limits::default()).await.unwrap();
        }
        assert_eq!(got, want, "{script}");
    }
}

#[tokio::test]
async fn later_results_carry_the_batch_writes_and_skips_skip_or_drain() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_off().await;
    let mut s = fx.session(SessionOptions::default()).await;
    run(&mut s, "CREATE TABLE dbo.k (n int)", Limits::default())
        .await
        .remove(0)
        .unwrap();
    {
        let mut sc =
            s.script("INSERT dbo.k VALUES (5)\nGO\nSELECT 1 AS a; SELECT 2 AS b; SELECT 3 AS c");
        sc.next().await.unwrap().unwrap().skip();
        let first = sc.next().await.unwrap().unwrap();
        assert_eq!(first.classification().writes, Writes::No);
        collect(first, Limits::default()).await.unwrap();
        let second = sc.next().await.unwrap().unwrap();
        assert!(
            second.classification().unguarded.is_empty()
                && !second.classification().transaction_control
        );
        second.skip();
        let third = collect(sc.next().await.unwrap().unwrap(), Limits::default())
            .await
            .unwrap();
        assert_eq!(third.rows, vec![vec![text("3")]]);
        assert!(sc.next().await.is_none());
    }
    let none = run(&mut s, "SELECT COUNT(*) AS c FROM dbo.k", Limits::default()).await;
    assert_eq!(
        none[0].as_ref().unwrap().rows,
        vec![vec![text("0")]],
        "the skipped INSERT never ran"
    );
}

#[tokio::test]
async fn an_error_nobody_read_surfaces_once_from_settle() {
    dbento_testkit::skip_without_mssql!();
    let fx = ms_off().await;
    let mut s = fx.session(SessionOptions::default()).await;
    for ran in run(
        &mut s,
        "CREATE TABLE dbo.u (n int PRIMARY KEY); INSERT dbo.u VALUES (1)",
        Limits::default(),
    )
    .await
    {
        ran.unwrap();
    }
    let settled = |r: Result<(), ExecuteError>| match r {
        Err(ExecuteError::Database { code, .. }) => code,
        other => panic!("{other:?}"),
    };
    {
        let mut sc = s.script("SELECT 1 AS a; INSERT dbo.u VALUES (1)");
        collect(sc.next().await.unwrap().unwrap(), Limits::default())
            .await
            .unwrap();
    }
    assert_eq!(
        settled(s.settle().await),
        "2627",
        "a dropped script's drain keeps its error"
    );
    s.settle().await.unwrap();
    {
        let mut sc = s.script("SELECT 1 AS a; INSERT dbo.u VALUES (1); SELECT 2 AS b");
        collect(sc.next().await.unwrap().unwrap(), Limits::default())
            .await
            .unwrap();
        sc.next().await.unwrap().unwrap().skip();
        assert!(
            sc.next().await.is_none(),
            "the skipped error still ends the script"
        );
    }
    assert_eq!(
        settled(s.settle().await),
        "2627",
        "a skipped error item is kept"
    );
    {
        let mut sc = s.script("SELECT 1 AS a; SELECT 1/0 AS boom");
        collect(sc.next().await.unwrap().unwrap(), Limits::default())
            .await
            .unwrap();
        sc.next().await.unwrap().unwrap().skip();
        assert!(sc.next().await.is_none());
    }
    assert_eq!(
        settled(s.settle().await),
        "8134",
        "an error in a skipped set's rest is kept"
    );
}
