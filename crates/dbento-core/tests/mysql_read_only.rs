//! Read-only MySQL sessions: writes fail inside a read-only transaction per statement,
//! transaction control is refused before it runs, multi-statement text never reaches the
//! server, and settle ends the transaction.
mod support;

use std::time::Duration;

use dbento_core::{ExecuteError, Limits, Session, SessionOptions, Unguarded, Writes};
use mysql_async::Conn;
use mysql_async::prelude::*;
use support::{MyFixture, Ran, collect, my_fixtures, my_thread_id, run, text};

fn forced() -> SessionOptions {
    let mut o = SessionOptions::default();
    o.force_read_only = true;
    o
}

async fn one(s: &mut Session, sql: &str) -> Result<Ran, ExecuteError> {
    run(s, sql, Limits::default()).await.pop().unwrap()
}

/// `t` with one row, and a session that is read-only through `opts` or the connection.
async fn setup(fx: &MyFixture, opts: SessionOptions) -> Session {
    let sql = "CREATE TABLE t (a INT PRIMARY KEY) ENGINE = InnoDB; INSERT INTO t VALUES (1)";
    fx.db.admin().await.query_drop(sql).await.unwrap();
    fx.session(opts).await
}

fn refused(r: Result<Ran, ExecuteError>) -> bool {
    matches!(r, Err(ExecuteError::ReadOnlyViolation { ref message }) if message.contains("READ ONLY"))
}

#[tokio::test]
async fn writes_and_ddl_are_refused_by_the_server_and_reads_run() {
    for (label, fx) in my_fixtures("").await {
        let mut s = setup(&fx, forced()).await;
        assert!(s.is_read_only(), "{label}");
        for sql in [
            "INSERT INTO t VALUES (2)",
            "UPDATE t SET a = 3 WHERE a = 1",
            "CREATE TABLE u (a INT)",
            "DROP TABLE t",
        ] {
            assert!(refused(one(&mut s, sql).await), "{label}: {sql}");
        }
        assert_eq!(
            one(&mut s, "SELECT a FROM t").await.unwrap().rows,
            vec![vec![text("1")]],
            "{label}"
        );
        // A user SET cannot lift read-only: the next statement re-sets it first.
        let _ = one(&mut s, "SET SESSION transaction_read_only = 0").await;
        assert!(
            refused(one(&mut s, "INSERT INTO t VALUES (4)").await),
            "{label}"
        );
        assert_eq!(
            fx.db.scalar("SELECT COUNT(*) FROM t").await.as_deref(),
            Some("1"),
            "{label}"
        );
    }
    for (label, fx) in my_fixtures("read_only = true").await {
        let mut s = setup(&fx, SessionOptions::default()).await;
        assert!(
            refused(one(&mut s, "INSERT INTO t VALUES (2)").await),
            "{label}: read_only = true"
        );
    }
}

#[tokio::test]
async fn a_write_is_classified_before_the_server_refuses_it() {
    for (label, fx) in my_fixtures("").await {
        let mut s = setup(&fx, forced()).await;
        // MCP `execute` reads a write's classification from a read-only session, then skips it.
        let mut script = s.script("DELETE FROM t");
        match script.next().await {
            Some(Ok(pending)) => {
                let c = pending.classification();
                assert_eq!(
                    (c.writes, c.unguarded.clone()),
                    (Writes::Yes, vec![Unguarded::DeleteWithoutWhere]),
                    "{label}"
                );
                pending.skip();
            }
            other => panic!("{label}: {:?}", other.map(|r| r.err())),
        }
        drop(script);
        assert!(refused(one(&mut s, "DELETE FROM t").await), "{label}");
        assert_eq!(
            fx.db.scalar("SELECT COUNT(*) FROM t").await.as_deref(),
            Some("1"),
            "{label}"
        );
    }
}

#[tokio::test]
async fn transaction_control_is_refused_before_it_runs() {
    for (label, fx) in my_fixtures("").await {
        let mut s = setup(&fx, forced()).await;
        for sql in [
            "BEGIN",
            "START TRANSACTION",
            "COMMIT",
            "ROLLBACK",
            "SAVEPOINT x",
            "SET TRANSACTION READ WRITE",
            "SET SESSION TRANSACTION READ WRITE",
            "LOCK TABLES t READ",
            "UNLOCK TABLES",
            "XA START 'x'",
        ] {
            let r = one(&mut s, sql).await;
            assert!(
                matches!(r, Err(ExecuteError::TransactionControl { .. })),
                "{label}: {sql}"
            );
        }
        assert_eq!(
            one(&mut s, "SELECT 7").await.unwrap().rows,
            vec![vec![text("7")]],
            "{label}"
        );
    }
}

#[tokio::test]
async fn multi_statement_text_never_reaches_the_server() {
    for (label, fx) in my_fixtures("").await {
        let mut s = setup(&fx, forced()).await;
        // One statement to the splitter; `DO` fails the parser, so classification sees no
        // COMMIT. As text, the server would commit, flip the session and write.
        // Led by a write too: the parse fails (1064) before any read-only refusal (1792).
        for sql in [
            "DELIMITER //\nSELECT 1; DO 1; COMMIT; SET SESSION TRANSACTION READ WRITE; \
             INSERT INTO t VALUES (2)//",
            "DELIMITER //\nDELETE FROM t; DO 1; COMMIT; SET SESSION TRANSACTION READ WRITE; \
             INSERT INTO t VALUES (2)//",
        ] {
            match one(&mut s, sql).await {
                Err(ExecuteError::Database { code, .. }) => assert_eq!(code, "1064", "{label}"),
                other => panic!("{label}: {:?}", other.err()),
            }
        }
        assert_eq!(
            fx.db.scalar("SELECT COUNT(*) FROM t").await.as_deref(),
            Some("1"),
            "{label}"
        );
        // An unpreparable single statement (1295 on MySQL) still runs.
        let db = fx.db.database().to_owned();
        assert!(one(&mut s, &format!("USE `{db}`")).await.is_ok(), "{label}");
        assert_eq!(
            one(&mut s, "SELECT DATABASE()").await.unwrap().rows,
            vec![vec![text(&db)]],
            "{label}"
        );
    }
}

#[tokio::test]
async fn settle_ends_the_read_only_transaction() {
    for (label, fx) in my_fixtures("").await {
        let mut s = setup(&fx, forced()).await;
        assert!(one(&mut s, "SELECT a FROM t").await.is_ok(), "{label}");
        s.settle().await.unwrap();
        // An open transaction would hold t's metadata lock and stall this past 3 s.
        let mut admin = fx.db.admin().await;
        admin
            .query_drop("SET SESSION lock_wait_timeout = 3")
            .await
            .unwrap();
        admin
            .query_drop("ALTER TABLE t ADD COLUMN z INT")
            .await
            .unwrap();
    }
}

/// Polls the processlist until a thread matches `filter`, so a test acts only once a
/// statement is known to be waiting.
async fn wait_for(fx: &MyFixture, filter: &str) {
    let sql = format!("SELECT COUNT(*) FROM information_schema.PROCESSLIST WHERE {filter}");
    for _ in 0..400 {
        if fx.db.scalar(&sql).await.as_deref() != Some("0") {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("no thread matched {filter}");
}

/// Holds `t`'s metadata lock in an open transaction and queues `ALTER TABLE t ADD <col>`
/// behind it, so every later statement on `t` waits; returns the holder and the ALTER.
async fn block_t(fx: &MyFixture, col: &str) -> (Conn, tokio::task::JoinHandle<()>) {
    let mut holder = fx.db.admin().await;
    holder.query_drop("START TRANSACTION").await.unwrap();
    holder.query_drop("SELECT a FROM t").await.unwrap();
    let mut alter = fx.db.admin().await;
    alter
        .query_drop("SET SESSION lock_wait_timeout = 60")
        .await
        .unwrap();
    let sql = format!("ALTER TABLE t ADD COLUMN {col} INT");
    let queued = tokio::spawn(async move { alter.query_drop(sql).await.unwrap() });
    let db = fx.db.database();
    wait_for(fx, &format!("DB = '{db}' AND INFO LIKE 'ALTER TABLE t%' AND STATE LIKE 'Waiting for table metadata lock'")).await;
    (holder, queued)
}

#[tokio::test]
async fn a_prepare_stuck_behind_ddl_stops_at_its_timeout_or_cancel() {
    for (label, fx) in my_fixtures("").await {
        setup(&fx, forced()).await.close().await.unwrap();
        for timed in [true, false] {
            let mut opts = forced();
            if timed {
                opts.statement_timeout = Some(Duration::from_secs(1));
            }
            let mut s = fx.session(opts).await;
            let id = my_thread_id(&mut s).await;
            let (mut holder, alter) = block_t(&fx, if timed { "z1" } else { "z2" }).await;
            let cancel = s.cancel_handle();
            let stop = async {
                if !timed {
                    wait_for(
                        &fx,
                        &format!("ID = {id} AND STATE LIKE 'Waiting for table metadata lock'"),
                    )
                    .await;
                    cancel.cancel();
                }
            };
            let work = futures_util::future::join(one(&mut s, "SELECT a FROM t"), stop);
            let (r, ()) = tokio::time::timeout(Duration::from_secs(20), work)
                .await
                .unwrap_or_else(|_| panic!("{label}: the prepare was never stopped"));
            let want = if timed { "Timeout" } else { "Cancelled" };
            assert!(
                matches!(
                    (&r, timed),
                    (Err(ExecuteError::Timeout), true) | (Err(ExecuteError::Cancelled), false)
                ),
                "{label}: want {want}, got {:?}",
                r.err()
            );
            holder.query_drop("ROLLBACK").await.unwrap();
            alter.await.unwrap();
            assert_eq!(
                one(&mut s, "SELECT 7").await.unwrap().rows,
                vec![vec![text("7")]],
                "{label}"
            );
            s.close().await.unwrap();
        }
    }
}

#[tokio::test]
async fn the_read_only_transaction_ends_with_its_result() {
    for (label, fx) in my_fixtures("").await {
        let mut s = setup(&fx, forced()).await;
        let cases = [
            ("SELECT a FROM t", true),
            ("SELECT a INTO @x FROM t", true),
            (
                "SELECT a FROM t WHERE a = (SELECT a FROM t UNION ALL SELECT a FROM t)",
                false,
            ),
        ];
        for (i, (sql, ok)) in cases.into_iter().enumerate() {
            let mut script = s.script(sql);
            let p = script.next().await.unwrap().unwrap();
            let r = collect(p, Limits::default()).await;
            assert_eq!(r.is_ok(), ok, "{label}: {sql}: {:?}", r.err());
            // No settle has run: the script has not been asked for its next statement.
            let mut admin = fx.db.admin().await;
            admin
                .query_drop("SET SESSION lock_wait_timeout = 2")
                .await
                .unwrap();
            let altered = admin
                .query_drop(format!("ALTER TABLE t ADD COLUMN c{i} INT"))
                .await;
            assert!(altered.is_ok(), "{label}: {sql}: {:?}", altered.err());
            drop(script);
        }
        s.close().await.unwrap();
    }
}

#[tokio::test]
async fn the_prepare_and_its_execute_share_one_timeout() {
    for (label, fx) in my_fixtures("").await {
        setup(&fx, forced()).await.close().await.unwrap();
        let mut opts = forced();
        opts.statement_timeout = Some(Duration::from_secs(2));
        let mut s = fx.session(opts).await;
        let id = my_thread_id(&mut s).await;
        // The prepare waits 1.4 s behind DDL and the execute sleeps 1.4 s: each fits in the
        // timeout, together they do not.
        let (mut holder, alter) = block_t(&fx, "z3").await;
        let release = async {
            let waiting = format!("ID = {id} AND STATE LIKE 'Waiting for table metadata lock'");
            wait_for(&fx, &waiting).await;
            tokio::time::sleep(Duration::from_millis(1400)).await;
            holder.query_drop("ROLLBACK").await.unwrap();
        };
        let work = one(&mut s, "SELECT SLEEP(1.4) FROM t");
        let (r, ()) = futures_util::future::join(work, release).await;
        assert!(
            matches!(r, Err(ExecuteError::Timeout)),
            "{label}: {:?}",
            r.map(|r| r.rows)
        );
        alter.await.unwrap();
        // A cancel between the prepare and its execute still stops the statement.
        let cancel = s.cancel_handle();
        {
            let mut script = s.script("SELECT 7");
            let pending = script.next().await.unwrap().unwrap();
            cancel.cancel();
            let sent = pending.execute(Limits::default()).await.err();
            assert!(matches!(sent, Some(ExecuteError::Cancelled)), "{label}");
        }
        let again = one(&mut s, "SELECT 7").await.unwrap();
        assert_eq!(again.rows, vec![vec![text("7")]], "{label}");
        s.close().await.unwrap();
    }
}
