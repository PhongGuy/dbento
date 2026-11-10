//! Research A6, row by row: what each read-only layer blocks on SQL Server, and the holes
//! that remain, with and without a reader principal.
use std::time::Duration;

use crate::support::{MsFixture, Ran, collect, ms_reader, ms_until, run, text};
use dbento_core::{ExecuteError, Limits, ReadOnlyEnforcement, Session, SessionOptions, Writes};

const A6: &[&str] = &[
    "CREATE TABLE dbo.t (id int IDENTITY PRIMARY KEY, v nvarchar(40))",
    "INSERT dbo.t (v) VALUES (N'seed')",
    "CREATE STATISTICS st_v ON dbo.t (v)",
    "CREATE SEQUENCE dbo.seq START WITH 1",
    "SELECT NEXT VALUE FOR dbo.seq",
    "CREATE PROCEDURE dbo.p_pub AS INSERT dbo.t (v) VALUES (N'pub')",
    "CREATE PROCEDURE dbo.p_pub_commit AS BEGIN INSERT dbo.t (v) VALUES (N'pub commit'); COMMIT; COMMIT; END",
    "GRANT EXECUTE ON dbo.p_pub TO public",
    "GRANT EXECUTE ON dbo.p_pub_commit TO public",
    "CREATE TABLE dbo.trg (x int)",
    "CREATE TRIGGER dbo.trg_rb ON dbo.trg AFTER INSERT AS BEGIN ROLLBACK; INSERT dbo.t (v) VALUES (N'trigger'); END",
];
const STATS: &str = "SELECT CONVERT(nvarchar(30), STATS_DATE(OBJECT_ID(N'dbo.t'), \
                     (SELECT stats_id FROM sys.stats WHERE name = N'st_v')), 121)";
const SEQ: &str =
    "SELECT CAST(current_value AS nvarchar(20)) FROM sys.sequences WHERE name = N'seq'";
const IDENT: &str = "SELECT CAST(IDENT_CURRENT(N'dbo.t') AS nvarchar(20))";
const APPLOCK: &str = "@Resource = N'a6', @LockMode = N'Exclusive', @LockOwner = N'Session'";

type Results = Vec<Result<Ran, ExecuteError>>;

async fn session(extra: &str) -> (MsFixture, Session) {
    let fx = ms_reader(A6, &format!("read_only = true\n{extra}")).await;
    let s = fx.session(SessionOptions::default()).await;
    (fx, s)
}

async fn n(fx: &MsFixture, v: &str) -> String {
    let sql = format!("SELECT COUNT(*) FROM dbo.t WHERE v = N'{v}'");
    fx.db.scalar(&sql).await.unwrap()
}

fn last_err(r: &Results) -> Option<&ExecuteError> {
    r.last().and_then(|x| x.as_ref().err())
}

fn violation(r: &Results) -> bool {
    matches!(last_err(r), Some(ExecuteError::ReadOnlyViolation { .. }))
}

fn code(r: &Results) -> Option<&str> {
    match last_err(r) {
        Some(ExecuteError::Database { code, .. }) => Some(code),
        _ => None,
    }
}

/// Runs `sql` after checking the classifier cannot parse it, so only the fence stands in its way.
async fn unparsed(s: &mut Session, sql: &str) -> Results {
    let mut out = Vec::new();
    let mut script = s.script(sql);
    let mut first = true;
    while let Some(next) = script.next().await {
        let r = match next {
            Ok(p) => {
                if std::mem::take(&mut first) {
                    assert_eq!(p.classification().writes, Writes::Unknown, "{sql}");
                }
                collect(p, Limits::default()).await
            }
            Err(e) => Err(e),
        };
        let failed = r.is_err();
        out.push(r);
        if failed {
            break;
        }
    }
    out
}

/// Asserts layer 3 would let `sql` through, without ever executing it on the shared server.
async fn not_refused(s: &mut Session, sql: &str) {
    let mut script = s.script(sql);
    let p = script.next().await.unwrap().unwrap();
    assert_eq!(p.classification().writes, Writes::Unknown, "{sql}");
    assert!(!p.classification().transaction_control, "{sql}");
}

/// Asserts no verdict came with `r` or is owed after it: a commit here stays unreported.
async fn nothing_reported(s: &mut Session, r: &Results) {
    let verdict =
        |x: &Result<Ran, ExecuteError>| matches!(x, Err(ExecuteError::ReadOnlyViolation { .. }));
    assert!(!r.iter().any(verdict), "no read-only verdict is reported");
    s.settle().await.unwrap();
}

#[tokio::test]
async fn without_a_reader_the_a6_table_holds_row_by_row() {
    dbento_testkit::skip_without_mssql!();
    let (fx, mut s) = session("").await;
    let l = Limits::default();
    assert_eq!(
        s.read_only_enforcement(),
        Some(ReadOnlyEnforcement::BestEffort)
    );
    // DML and DDL: refused when parsed, executed then undone when not.
    for sql in [
        "INSERT dbo.t (v) VALUES (N'x')",
        "UPDATE dbo.t SET v = N'x' WHERE id = 1",
        "DELETE FROM dbo.t WHERE id = 1",
        "MERGE dbo.t AS d USING (SELECT 1 AS id) AS s ON d.id = s.id WHEN MATCHED THEN DELETE;",
        "TRUNCATE TABLE dbo.t",
        "CREATE TABLE dbo.n (a int)",
    ] {
        assert!(violation(&run(&mut s, sql, l).await), "{sql}");
    }
    // A verdict can also come after a run, so the refusal is proved by what the server holds.
    assert_eq!(n(&fx, "x").await, "0");
    assert_eq!(fx.db.scalar("SELECT OBJECT_ID(N'dbo.n')").await, None);
    let r = unparsed(&mut s, "DELETE TOP (1) FROM dbo.t").await;
    assert_eq!(r[0].as_ref().unwrap().outcome.rows_affected, Some(1));
    assert_eq!(n(&fx, "seed").await, "1");
    // Database-level statements are stopped by the fence's transaction.
    let backup = format!(
        "BACKUP DATABASE [{}] TO DISK = N'/tmp/dbento-a6.bak'",
        fx.db.database()
    );
    for (sql, want) in [
        ("ALTER DATABASE CURRENT SET RECOVERY SIMPLE", "226"),
        (backup.as_str(), "3021"),
        (
            "RESTORE DATABASE [dbento_a6_gone] FROM DISK = N'/tmp/dbento-a6-gone.bak'",
            "3021",
        ),
        ("RECONFIGURE", "574"),
        ("CREATE FULLTEXT CATALOG dbento_a6_ftc", "574"),
        ("SELECT 1 AS one CREATE DATABASE dbento_a6_gone", "226"),
        ("SELECT 1 AS one DROP DATABASE dbento_a6_gone", "574"),
    ] {
        assert_eq!(code(&unparsed(&mut s, sql).await), Some(want), "{sql}");
    }
    // Transaction control is refused, balanced or not, in every spelling that turns it off.
    for sql in [
        "BEGIN TRAN; COMMIT",
        "ROLLBACK",
        "SAVE TRAN s",
        "SET IMPLICIT_TRANSACTIONS OFF; ROLLBACK; INSERT dbo.t (v) VALUES (N'f4d')",
        "SET ANSI_DEFAULTS OFF EXEC('ROLLBACK') INSERT dbo.t (v) VALUES (N'f4d')",
        "SET NOCOUNT, IMPLICIT_TRANSACTIONS OFF EXEC('ROLLBACK') INSERT dbo.t (v) VALUES (N'f4d')",
        "SET SHOWPLAN_XML ON",
    ] {
        let r = run(&mut s, sql, l).await;
        assert!(
            matches!(last_err(&r), Some(ExecuteError::TransactionControl { .. })),
            "{sql}"
        );
    }
    assert_eq!(n(&fx, "f4d").await, "0");
    // Hole: two commits hidden in EXEC commit the write before them; the count reports it...
    let r = unparsed(
        &mut s,
        "INSERT dbo.t (v) VALUES (N'hidden') EXEC('COMMIT') EXEC('COMMIT')",
    )
    .await;
    assert!(violation(&r));
    assert_eq!(n(&fx, "hidden").await, "1");
    // ...and so does the descriptor when the batch restores the count...
    let r = unparsed(
        &mut s,
        "INSERT dbo.t (v) VALUES (N'restored') EXEC('COMMIT COMMIT BEGIN TRAN') SELECT 1 AS one",
    )
    .await;
    assert!(violation(&r));
    assert_eq!(n(&fx, "restored").await, "1");
    // ...but a write after the commits, or short of the second one, sits in a transaction the
    // close-out rolls back; the low count is still reported.
    for (v, sql) in [
        (
            "after",
            "EXEC('COMMIT') EXEC('COMMIT') INSERT dbo.t (v) VALUES (N'after')",
        ),
        (
            "one commit",
            "INSERT dbo.t (v) VALUES (N'one commit') EXEC('COMMIT')",
        ),
    ] {
        assert!(violation(&unparsed(&mut s, sql).await), "{sql}");
        assert_eq!(n(&fx, v).await, "0", "{sql}");
    }
    // A write before them, though, commits unreported if the batch then raises an error.
    for (v, commit) in [
        ("quiet", "EXEC('COMMIT') EXEC('COMMIT')"),
        ("quiet restored", "EXEC('COMMIT COMMIT BEGIN TRAN')"),
    ] {
        let sql = format!("INSERT dbo.t (v) VALUES (N'{v}') {commit} RAISERROR('late', 16, 1)");
        let r = unparsed(&mut s, &sql).await;
        assert_eq!(code(&r), Some("50000"), "{sql}");
        nothing_reported(&mut s, &r).await;
        assert_eq!(n(&fx, v).await, "1", "{sql}");
    }
    // Hole: an EXEC string turns implicit transactions off for itself; the server's own 266
    // suppresses the verdict, so this commits unreported with no user error in the batch.
    let r = unparsed(
        &mut s,
        "EXEC('ROLLBACK SET IMPLICIT_TRANSACTIONS OFF INSERT dbo.t (v) VALUES (N''off'')') SELECT 1 AS one",
    )
    .await;
    assert_eq!(code(&r), Some("266"));
    nothing_reported(&mut s, &r).await;
    assert_eq!(n(&fx, "off").await, "1");
    // Hole: a stop rolls back what is open, not what committed, and a stop is not judged.
    let cancel = s.cancel_handle();
    let stopper = async {
        let committed = "SELECT COUNT(*) FROM dbo.t WHERE v = N'stopped'";
        assert!(ms_until(&fx.db, committed, "1").await);
        cancel.cancel();
    };
    let batch = unparsed(
        &mut s,
        "EXEC('INSERT dbo.t (v) VALUES (N''stopped'') COMMIT COMMIT BEGIN TRAN') \
         WAITFOR DELAY '00:00:30'",
    );
    let (r, ()) = tokio::join!(batch, stopper);
    assert!(matches!(last_err(&r), Some(ExecuteError::Cancelled)));
    nothing_reported(&mut s, &r).await;
    assert_eq!(n(&fx, "stopped").await, "1");
    // A hidden ROLLBACK: the write after it is caught by implicit transactions and undone.
    let r = unparsed(
        &mut s,
        "EXEC('ROLLBACK') INSERT dbo.t (v) VALUES (N'after rollback')",
    )
    .await;
    assert!(violation(&r));
    assert_eq!(n(&fx, "after rollback").await, "0");
    // Not a hole: a trigger's write after its own ROLLBACK opens an implicit transaction,
    // which the close-out rolls back; the server raises nothing and the low count is reported.
    let r = unparsed(&mut s, "INSERT dbo.trg VALUES (1) SELECT 1 AS one").await;
    assert!(r[..r.len() - 1].iter().all(Result::is_ok));
    assert!(violation(&r));
    assert_eq!(n(&fx, "trigger").await, "0");
    // An error ends the fence's transaction; the next batch gets a new one.
    assert_eq!(
        code(&unparsed(&mut s, "SELECT 1/0 AS boom SELECT 2 AS two").await),
        Some("8134")
    );
    let r = unparsed(
        &mut s,
        "INSERT dbo.t (v) VALUES (N'next batch') SELECT 1 AS one",
    )
    .await;
    assert!(r.iter().all(Result::is_ok));
    assert_eq!(n(&fx, "next batch").await, "0");
    // Holes: statistics, sequences and identities move although the fence rolls back.
    for (probe, sql) in [
        (STATS, "UPDATE STATISTICS dbo.t st_v WITH FULLSCAN"),
        (SEQ, "SELECT NEXT VALUE FOR dbo.seq AS n"),
        (IDENT, "INSERT dbo.t (v) VALUES (N'ident') SELECT 1 AS one"),
    ] {
        let before = fx.db.scalar(probe).await;
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert!(
            unparsed(&mut s, sql).await.iter().all(Result::is_ok),
            "{sql}"
        );
        assert_ne!(fx.db.scalar(probe).await, before, "{sql}");
    }
    assert_eq!(n(&fx, "ident").await, "0");
    // Temp tables: A6 calls them harmless, but creating one parses as a write and is refused;
    // a table variable stays usable, even when an unparsed batch fills it.
    for sql in ["CREATE TABLE #tmp (x int)", "CREATE TABLE ##tmp (x int)"] {
        assert!(violation(&run(&mut s, sql, l).await), "{sql}");
    }
    let r = run(&mut s, "DECLARE @t TABLE (a int); SELECT a FROM @t", l).await;
    assert!(r.iter().all(Result::is_ok));
    let r = unparsed(
        &mut s,
        "DECLARE @t TABLE (a int) INSERT @t VALUES (1) SELECT a FROM @t",
    )
    .await;
    assert_eq!(
        r.last().unwrap().as_ref().unwrap().rows,
        vec![vec![text("1")]]
    );
    // WAITFOR is a read.
    let r = run(&mut s, "WAITFOR DELAY '00:00:00.100'; SELECT 1 AS one", l).await;
    assert_eq!(
        r.last().unwrap().as_ref().unwrap().rows,
        vec![vec![text("1")]]
    );
    // sp_getapplock is no write, but bare it is an EXEC and refused like any other; behind a
    // parse failure it runs, and a session lock outlives the fence's rollback.
    assert!(violation(
        &run(&mut s, &format!("EXEC sp_getapplock {APPLOCK}"), l).await
    ));
    let sql = format!("SELECT 1 AS one DECLARE @r int EXEC @r = sp_getapplock {APPLOCK}");
    assert!(unparsed(&mut s, &sql).await.iter().all(Result::is_ok));
    let r = run(
        &mut s,
        "SELECT APPLOCK_MODE(N'public', N'a6', N'Session') AS m",
        l,
    )
    .await;
    assert_eq!(r[0].as_ref().unwrap().rows, vec![vec![text("Exclusive")]]);
    // Server-level procedures and statements: EXEC and KILL are refused unsent...
    for sql in [
        "EXEC xp_cmdshell 'id'",
        "EXEC sp_OACreate 'x'",
        "EXEC msdb.dbo.sp_start_job @job_name = N'nope'",
        "EXEC msdb.dbo.sp_send_dbmail @body = N'x'",
        "KILL 1",
        "EXEC dbo.p_pub",
    ] {
        assert!(violation(&run(&mut s, sql, l).await), "{sql}");
    }
    assert_eq!(n(&fx, "pub").await, "0");
    // ...but not behind a parse failure, nor SHUTDOWN at all, so none of these is sent here.
    // A bare SHUTDOWN is 6007 in the fence.
    for sql in [
        "SHUTDOWN WITH NOWAIT",
        "EXEC('COMMIT COMMIT') SHUTDOWN WITH NOWAIT",
        "SELECT 1 AS one EXEC xp_cmdshell 'id'",
        "SELECT 1 AS one EXEC msdb.dbo.sp_start_job @job_name = N'nope'",
        "SELECT 1 AS one KILL 1",
    ] {
        not_refused(&mut s, sql).await;
    }
    // A6's "CLR external" has no row: SQL Server on Linux, which the gate server runs, does not
    // support EXTERNAL_ACCESS or UNSAFE assemblies, so there is nothing here to call.
    // A linked server is reached the same way; this server has none, so the name fails.
    assert_eq!(
        code(&unparsed(&mut s, "EXEC ('SELECT 1') AT dbento_a6_lnk").await),
        Some("7202")
    );
    // Hole: a public procedure's write is undone unless it commits, which is then reported...
    let r = unparsed(&mut s, "SELECT 1 AS one EXEC dbo.p_pub").await;
    assert!(r.iter().all(Result::is_ok));
    assert_eq!(n(&fx, "pub").await, "0");
    let r = unparsed(&mut s, "SELECT 1 AS one EXEC dbo.p_pub_commit").await;
    assert!(violation(&r));
    assert_eq!(n(&fx, "pub commit").await, "1");
    // ...and unreported when the batch then raises an error.
    let r = unparsed(
        &mut s,
        "SELECT 1 AS one EXEC dbo.p_pub_commit RAISERROR('late', 16, 1)",
    )
    .await;
    assert_eq!(code(&r), Some("50000"));
    nothing_reported(&mut s, &r).await;
    assert_eq!(n(&fx, "pub commit").await, "2");
    // Hole: DBCC maintenance and CHECKPOINT run as far as the login's privileges allow.
    for sql in ["DBCC FREEPROCCACHE", "CHECKPOINT"] {
        assert!(
            unparsed(&mut s, sql).await.iter().all(Result::is_ok),
            "{sql}"
        );
    }
}

#[tokio::test]
async fn with_a_reader_the_a6_table_holds_row_by_row() {
    dbento_testkit::skip_without_mssql!();
    let (fx, mut s) = session("read_only_user = \"dbento_reader\"").await;
    let l = Limits::default();
    // A second database on the same server: mssql_db would start another server, and it waits
    // for the slot this fixture holds.
    let other = format!("{}_other", fx.db.database());
    let create = format!("DROP DATABASE IF EXISTS [{other}]; CREATE DATABASE [{other}]");
    fx.db.scalar(&create).await;
    let secret = format!("CREATE TABLE [{other}].dbo.secret (x int)");
    fx.db.scalar(&secret).await;
    let stats = fx.db.scalar(STATS).await;
    let seq = fx.db.scalar(SEQ).await;
    // Permissions refuse writes, parsed or not, and server-level actions, before they run.
    let cross = format!("SELECT x FROM [{other}].dbo.secret");
    let enter = format!("USE [{other}]");
    for sql in [
        "INSERT dbo.t (v) VALUES (N'x')",
        "DELETE TOP (1) FROM dbo.t",
        "ALTER TABLE dbo.t ADD extra int",
        "UPDATE STATISTICS dbo.t st_v WITH FULLSCAN",
        "SELECT NEXT VALUE FOR dbo.seq AS n",
        "EXEC('COMMIT') EXEC('COMMIT') INSERT dbo.t (v) VALUES (N'late')",
        "EXEC xp_cmdshell 'id'",
        "EXEC msdb.dbo.sp_start_job @job_name = N'nope'",
        "KILL 1",
        "DBCC FREEPROCCACHE",
        "DBCC CHECKDB",
        "CHECKPOINT",
        cross.as_str(),
        enter.as_str(),
    ] {
        let r = run(&mut s, sql, l).await;
        assert!(violation(&r), "{sql}: {:?}", last_err(&r));
    }
    assert!(
        last_err(&run(&mut s, "ALTER DATABASE CURRENT SET RECOVERY SIMPLE", l).await).is_some()
    );
    assert_eq!(
        (n(&fx, "seed").await, n(&fx, "late").await),
        ("1".to_owned(), "0".to_owned())
    );
    assert_eq!(
        (fx.db.scalar(STATS).await, fx.db.scalar(SEQ).await),
        (stats, seq)
    );
    // Transaction control is still refused; temp tables stay usable.
    let r = run(&mut s, "BEGIN TRAN; COMMIT", l).await;
    assert!(matches!(
        last_err(&r),
        Some(ExecuteError::TransactionControl { .. })
    ));
    let r = run(
        &mut s,
        "CREATE TABLE #tmp (x int); INSERT #tmp VALUES (1); SELECT x AS ok FROM #tmp",
        l,
    )
    .await;
    assert_eq!(
        r.last().unwrap().as_ref().unwrap().rows,
        vec![vec![text("1")]]
    );
    // The impersonation cannot be undone or replaced.
    assert!(last_err(&run(&mut s, "REVERT", l).await).is_some());
    assert!(last_err(&run(&mut s, "EXECUTE AS USER = N'dbo'", l).await).is_some());
    let who = run(&mut s, "SELECT USER_NAME() AS u", l)
        .await
        .remove(0)
        .unwrap();
    assert_eq!(who.rows, vec![vec![text("dbento_reader")]]);
    // SHUTDOWN is denied by a message that also ends the batch and its transaction, so the low
    // count is reported though nothing committed.
    assert!(violation(&run(&mut s, "SHUTDOWN WITH NOWAIT", l).await));
    assert_eq!(fx.db.scalar("SELECT 1").await.as_deref(), Some("1"));
    // Hole: a procedure granted to public writes through ownership chaining; the fence undoes
    // it unless it commits.
    assert!(
        run(&mut s, "EXEC dbo.p_pub", l)
            .await
            .iter()
            .all(Result::is_ok)
    );
    assert_eq!(n(&fx, "pub").await, "0");
    assert!(violation(&run(&mut s, "EXEC dbo.p_pub_commit", l).await));
    assert_eq!(n(&fx, "pub commit").await, "1");
    fx.db.scalar(&format!("DROP DATABASE [{other}]")).await;
}
