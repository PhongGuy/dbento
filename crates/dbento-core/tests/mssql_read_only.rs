//! Read-only SQL Server without a reader principal: Dbento's transaction fence around every
//! batch and the refusals before sending, reported as best effort.
mod support;

use dbento_core::{
    ExecuteError, Limits, OnTruncate, ReadOnlyEnforcement, RelationRef, Session, SessionOptions,
    Writes,
};
use support::{MsFixture, collect, ms_fixture, ms_off, run, sqlite_fixture, text};

async fn read_only() -> (MsFixture, Session) {
    let fx = ms_fixture(
        dbento_testkit::mssql_db().await,
        "tls = \"off\"\nread_only = true",
    );
    fx.db
        .scalar("CREATE TABLE dbo.t (id int IDENTITY PRIMARY KEY, v nvarchar(20))")
        .await;
    let s = fx.session(SessionOptions::default()).await;
    (fx, s)
}

async fn n(fx: &MsFixture, v: &str) -> String {
    fx.db
        .scalar(&format!("SELECT COUNT(*) FROM dbo.t WHERE v = N'{v}'"))
        .await
        .unwrap()
}

async fn trancount(s: &mut Session) -> Vec<Vec<support::Cell>> {
    run(s, "SELECT @@TRANCOUNT AS seen", Limits::default())
        .await
        .remove(0)
        .unwrap()
        .rows
}

#[tokio::test]
async fn a_read_only_session_is_best_effort_and_each_batch_sees_dbentos_transaction() {
    dbento_testkit::skip_without_mssql!();
    // Testkit runs one container at a time, so the writable one goes before the read-only one.
    {
        let writable = ms_off().await;
        assert_eq!(
            writable
                .session(SessionOptions::default())
                .await
                .read_only_enforcement(),
            None
        );
    }
    let (_fx, mut s) = read_only().await;
    assert_eq!(
        s.read_only_enforcement(),
        Some(ReadOnlyEnforcement::BestEffort)
    );
    let twice = run(
        &mut s,
        "SELECT @@TRANCOUNT AS seen\nGO 2",
        Limits::default(),
    )
    .await;
    assert_eq!(twice.len(), 2);
    for r in twice {
        assert_eq!(r.unwrap().rows, vec![vec![text("2")]]);
    }
    s.columns(&RelationRef::new("dbo", "t")).await.unwrap();
    assert_eq!(trancount(&mut s).await, vec![vec![text("2")]]);
}

#[tokio::test]
async fn other_engines_report_database_enforcement() {
    let fx = sqlite_fixture("CREATE TABLE t (a)", "read_only = true");
    let s = fx.session(SessionOptions::default()).await;
    assert_eq!(
        s.read_only_enforcement(),
        Some(ReadOnlyEnforcement::Database)
    );
    let fx = sqlite_fixture("CREATE TABLE t (a)", "");
    assert_eq!(
        fx.session(SessionOptions::default())
            .await
            .read_only_enforcement(),
        None
    );
}

#[tokio::test]
async fn writes_and_transaction_control_are_refused_at_execute_before_sending() {
    dbento_testkit::skip_without_mssql!();
    let (fx, mut s) = read_only().await;
    // `Next` hands out the classification; executing the batch is what is refused.
    {
        let mut script = s.script("DELETE FROM dbo.t");
        let p = script.next().await.unwrap().unwrap();
        assert_eq!(p.classification().writes, Writes::Yes);
        assert!(!p.classification().unguarded.is_empty());
        assert!(matches!(
            collect(p, Limits::default()).await,
            Err(ExecuteError::ReadOnlyViolation { .. })
        ));
        assert!(script.next().await.is_none(), "a refusal ends the script");
    }
    {
        let mut script = s.script("BEGIN TRANSACTION");
        let p = script.next().await.unwrap().unwrap();
        assert!(p.classification().transaction_control);
        assert!(matches!(
            collect(p, Limits::default()).await,
            Err(ExecuteError::TransactionControl { .. })
        ));
    }
    match run(&mut s, "INSERT dbo.t (v) VALUES (N'r1')", Limits::default())
        .await
        .remove(0)
    {
        Err(ExecuteError::ReadOnlyViolation { message }) => {
            assert!(message.contains("read_only_user"), "{message}")
        }
        other => panic!("{:?}", other.err()),
    }
    assert_eq!(n(&fx, "r1").await, "0");
    for sql in [
        "BEGIN TRANSACTION",
        "COMMIT",
        "SET IMPLICIT_TRANSACTIONS OFF; SELECT 1 AS one",
        "SET SHOWPLAN_XML ON",
        "SELECT 1 AS one; SAVE TRAN s",
    ] {
        let r = run(&mut s, sql, Limits::default()).await.remove(0);
        assert!(
            matches!(r, Err(ExecuteError::TransactionControl { .. })),
            "{sql}: {:?}",
            r.err()
        );
    }
    // A batch the parser cannot read is not refused for that alone.
    let both = run(&mut s, "SELECT 1 AS a SELECT 2 AS b", Limits::default()).await;
    assert_eq!(both.len(), 2);
    assert!(both.iter().all(Result::is_ok));
}

#[tokio::test]
async fn an_unparsed_write_runs_inside_the_fence_and_is_rolled_back() {
    dbento_testkit::skip_without_mssql!();
    let (fx, mut s) = read_only().await;
    {
        let mut script = s.script("INSERT dbo.t (v) VALUES (N'u1') SELECT 1 AS one");
        let p = script.next().await.unwrap().unwrap();
        assert_eq!(p.classification().writes, Writes::Unknown);
        assert_eq!(
            collect(p, Limits::default())
                .await
                .unwrap()
                .outcome
                .rows_affected,
            Some(1)
        );
        let p = script.next().await.unwrap().unwrap();
        assert_eq!(
            collect(p, Limits::default()).await.unwrap().rows,
            vec![vec![text("1")]]
        );
        assert!(script.next().await.is_none());
    }
    assert_eq!(n(&fx, "u1").await, "0");
}

#[tokio::test]
async fn a_cancelled_batch_is_closed_out_and_its_low_count_is_not_judged() {
    dbento_testkit::skip_without_mssql!();
    let (fx, mut s) = read_only().await;
    let cancel = s.cancel_handle();
    let t = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(500));
        cancel.cancel();
    });
    let sql = "INSERT dbo.t (v) VALUES (N'c1') WAITFOR DELAY '00:00:30'";
    let r = run(&mut s, sql, Limits::default()).await;
    t.join().unwrap();
    match r.last() {
        Some(Err(ExecuteError::Cancelled)) => {}
        other => panic!("{:?}", other.map(|x| x.as_ref().err())),
    }
    assert_eq!(n(&fx, "c1").await, "0");
    assert_eq!(trancount(&mut s).await, vec![vec![text("2")]]);
    s.settle().await.unwrap();
}

// A rollback's ENVCHANGE can sit in the part of a response that a stop discards.
#[tokio::test]
async fn the_session_stays_fenced_through_repeated_cancels() {
    dbento_testkit::skip_without_mssql!();
    let (_fx, mut s) = read_only().await;
    for sql in [
        "INSERT dbo.t (v) VALUES (N'c2') WAITFOR DELAY '00:00:30'",
        "SELECT a.object_id FROM sys.all_objects a CROSS JOIN sys.all_objects b \
         CROSS JOIN sys.all_objects c",
    ] {
        for round in 0..5 {
            let cancel = s.cancel_handle();
            let t = std::thread::spawn(move || {
                std::thread::sleep(std::time::Duration::from_millis(300));
                cancel.cancel();
            });
            let r = run(&mut s, sql, Limits::default()).await;
            t.join().unwrap();
            assert!(
                matches!(r.last(), Some(Err(ExecuteError::Cancelled))),
                "{sql} {round}"
            );
            assert_eq!(
                trancount(&mut s).await,
                vec![vec![text("2")]],
                "{sql} {round}"
            );
        }
    }
    s.settle().await.unwrap();
}

const CUBE: &str = "SELECT a.object_id FROM sys.all_objects a CROSS JOIN sys.all_objects b \
                    CROSS JOIN sys.all_objects c";

fn closing_at(rows: u64) -> Limits {
    let mut l = Limits::default();
    l.max_rows = Some(rows);
    l.on_truncate = OnTruncate::Close;
    l
}

/// Runs `sql`'s first result under `closing_at(2)`, as MCP does, and returns what closing reports.
async fn cut_then_close(mut s: Session, sql: &str) -> Result<(), ExecuteError> {
    {
        let mut script = s.script(sql);
        let p = script.next().await.unwrap().unwrap();
        let ran = collect(p, closing_at(2)).await.unwrap();
        assert!(ran.outcome.truncated);
    }
    s.close().await
}

fn is_hidden_commit(r: Result<(), ExecuteError>) -> bool {
    matches!(r, Err(ExecuteError::ReadOnlyViolation { message })
        if message.contains("may have committed"))
}

#[tokio::test]
async fn a_hidden_commit_before_a_cut_under_close_is_reported() {
    dbento_testkit::skip_without_mssql!();
    let (fx, s) = read_only().await;
    let sql =
        format!("SET NOCOUNT ON EXEC('INSERT dbo.t (v) VALUES (N''k1'') COMMIT COMMIT') {CUBE}");
    let closed = cut_then_close(s, &sql).await;
    assert!(is_hidden_commit(closed));
    assert_eq!(n(&fx, "k1").await, "1");
}

#[tokio::test]
async fn a_hidden_commit_after_a_cut_in_a_whole_response_is_reported() {
    dbento_testkit::skip_without_mssql!();
    let (fx, s) = read_only().await;
    let sql = "SET NOCOUNT ON SELECT TOP (5) object_id FROM sys.all_objects \
               EXEC('INSERT dbo.t (v) VALUES (N''k2'') COMMIT COMMIT')";
    let closed = cut_then_close(s, sql).await;
    assert!(is_hidden_commit(closed));
    assert_eq!(n(&fx, "k2").await, "1");
}

#[tokio::test]
async fn a_plain_cut_under_close_reports_nothing() {
    dbento_testkit::skip_without_mssql!();
    let (_fx, s) = read_only().await;
    let closed = cut_then_close(s, CUBE).await;
    closed.unwrap();
}

#[tokio::test]
async fn a_plain_cut_of_a_whole_response_under_close_reports_nothing() {
    dbento_testkit::skip_without_mssql!();
    let (_fx, s) = read_only().await;
    let closed = cut_then_close(s, "SELECT TOP (5) object_id FROM sys.all_objects").await;
    closed.unwrap();
}

#[tokio::test]
async fn a_commit_that_restores_the_count_is_reported() {
    dbento_testkit::skip_without_mssql!();
    let (fx, mut s) = read_only().await;
    let sql = "EXEC('INSERT dbo.t (v) VALUES (N''k3'') COMMIT COMMIT BEGIN TRAN') SELECT 1 AS one";
    let r = run(&mut s, sql, Limits::default()).await;
    match r.last() {
        Some(Err(ExecuteError::ReadOnlyViolation { message })) => {
            assert!(message.contains("may have committed"), "{message}")
        }
        other => panic!("{:?}", other.map(|x| x.as_ref().err())),
    }
    assert_eq!(n(&fx, "k3").await, "1");
}

#[tokio::test]
async fn a_low_count_is_owed_once_the_script_is_gone() {
    dbento_testkit::skip_without_mssql!();
    let (_fx, mut s) = read_only().await;
    {
        let mut script = s.script("EXEC('ROLLBACK') SELECT 1 AS a SELECT 2 AS b");
        let p = script.next().await.unwrap().unwrap();
        collect(p, Limits::default()).await.unwrap();
    }
    match s.settle().await {
        Err(ExecuteError::ReadOnlyViolation { message }) => {
            assert!(message.contains("may have committed"), "{message}")
        }
        other => panic!("{other:?}"),
    }
    s.settle().await.unwrap();
}

#[tokio::test]
async fn a_failed_batch_whose_script_is_gone_owes_its_error_and_no_verdict() {
    dbento_testkit::skip_without_mssql!();
    let (_fx, mut s) = read_only().await;
    {
        let mut script = s.script("SELECT 1 AS a SELECT 1/0 AS b");
        let p = script.next().await.unwrap().unwrap();
        collect(p, Limits::default()).await.unwrap();
    }
    assert!(matches!(
        s.settle().await,
        Err(ExecuteError::Database { ref code, .. }) if code == "8134"
    ));
    s.settle().await.unwrap();
    assert_eq!(trancount(&mut s).await, vec![vec![text("2")]]);
}

// Each switch would let the INSERT commit if the fence let it through.
#[tokio::test]
async fn every_spelling_that_turns_implicit_transactions_off_is_refused() {
    dbento_testkit::skip_without_mssql!();
    let (fx, mut s) = read_only().await;
    for switch in [
        "SET ANSI_DEFAULTS OFF",
        "SET NOCOUNT, IMPLICIT_TRANSACTIONS OFF",
        "SET NOCOUNT, ANSI_DEFAULTS OFF",
    ] {
        let sql =
            format!("{switch} EXEC('ROLLBACK') INSERT dbo.t (v) VALUES (N's1') SELECT 1 AS one");
        let r = run(&mut s, &sql, Limits::default()).await.remove(0);
        assert!(
            matches!(r, Err(ExecuteError::TransactionControl { .. })),
            "{sql}: {:?}",
            r.err()
        );
    }
    assert_eq!(n(&fx, "s1").await, "0");
}

// The server reads a token break in each, so the fence must too.
#[tokio::test]
async fn transaction_control_behind_a_token_break_only_the_server_reads_is_refused() {
    dbento_testkit::skip_without_mssql!();
    let (fx, mut s) = read_only().await;
    let write = "EXEC('ROLLBACK') INSERT dbo.t (v) VALUES (N's2') SELECT 1 AS one";
    for sql in [
        format!("SET\u{A0}IMPLICIT_TRANSACTIONS OFF {write}"),
        format!("--c\rSET IMPLICIT_TRANSACTIONS OFF {write}"),
        format!("SELECT 1SET IMPLICIT_TRANSACTIONS OFF {write}"),
        format!("SELECT $1SET IMPLICIT_TRANSACTIONS OFF {write}"),
        "SELECT 1\u{A0}COMMIT\u{A0}COMMIT\u{A0}BEGIN\u{A0}TRAN".to_owned(),
        "SELECT 0xFFSAVE TRAN s".to_owned(),
    ] {
        let r = run(&mut s, &sql, Limits::default()).await.remove(0);
        assert!(
            matches!(r, Err(ExecuteError::TransactionControl { .. })),
            "{sql:?}: {:?}",
            r.err()
        );
    }
    assert_eq!(n(&fx, "s2").await, "0");
    let ran = run(&mut s, "SELECT 1\u{A0}AS one", Limits::default())
        .await
        .remove(0)
        .unwrap();
    assert_eq!(ran.rows, vec![vec![text("1")]]);
}

#[tokio::test]
async fn a_batch_that_ends_dbentos_transaction_is_reported() {
    dbento_testkit::skip_without_mssql!();
    let (fx, mut s) = read_only().await;
    let r = run(
        &mut s,
        "EXEC('ROLLBACK') INSERT dbo.t (v) VALUES (N'h1')",
        Limits::default(),
    )
    .await;
    assert_eq!(r[0].as_ref().unwrap().outcome.rows_affected, Some(1));
    match r.last() {
        Some(Err(ExecuteError::ReadOnlyViolation { message })) => {
            assert!(message.contains("may have committed"), "{message}")
        }
        other => panic!("{:?}", other.map(|x| x.as_ref().err())),
    }
    assert_eq!(n(&fx, "h1").await, "0");
    assert_eq!(trancount(&mut s).await, vec![vec![text("2")]]);
}

#[tokio::test]
async fn the_close_out_runs_after_errors_cuts_and_session_switches() {
    dbento_testkit::skip_without_mssql!();
    let (_fx, mut s) = read_only().await;
    let boom = run(&mut s, "SELECT 1/0 AS boom", Limits::default())
        .await
        .remove(0);
    assert!(matches!(boom, Err(ExecuteError::Database { ref code, .. }) if code == "8134"));
    assert_eq!(trancount(&mut s).await, vec![vec![text("2")]]);
    for switch in ["SET NOEXEC ON", "SET FMTONLY ON", "SET PARSEONLY ON"] {
        run(&mut s, switch, Limits::default())
            .await
            .remove(0)
            .unwrap();
        let after = run(&mut s, "SELECT 7 AS seven", Limits::default())
            .await
            .remove(0)
            .unwrap();
        assert_eq!(after.rows, vec![vec![text("7")]], "{switch}");
    }
    let mut cut = Limits::default();
    cut.max_rows = Some(2);
    let ran = run(
        &mut s,
        "SELECT TOP (5000) a.object_id FROM sys.all_objects a CROSS JOIN sys.all_objects b",
        cut,
    )
    .await
    .remove(0)
    .unwrap();
    assert!(ran.outcome.truncated);
    assert_eq!(trancount(&mut s).await, vec![vec![text("2")]]);
}

#[tokio::test]
async fn a_connection_states_the_level_its_read_only_session_reports() {
    dbento_testkit::skip_without_mssql!();
    let (fx, s) = read_only().await;
    assert_eq!(
        Some(fx.conn().read_only_enforcement()),
        s.read_only_enforcement()
    );
}
