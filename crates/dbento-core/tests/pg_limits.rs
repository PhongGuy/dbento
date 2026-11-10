//! The drain cap, close with rows unread, and how 57014 is reported.
use std::future::Future;
use std::time::{Duration, Instant};

use crate::support::{PgFixture, collect, pg_fixture, run, text};
use dbento_core::{ExecuteError, Limits, SessionOptions};

/// About 30 s of 10 kB rows; each row is flushed as it is produced.
const SLOW: &str = "SELECT repeat('x', 10000), pg_sleep(0.1) FROM generate_series(1, 300)";

/// Fails a test that hangs instead of letting it run forever.
async fn bounded(test: impl Future<Output = ()>) {
    tokio::time::timeout(Duration::from_secs(90), test)
        .await
        .expect("the test did not finish within 90 s");
}

/// Whether the server has no session of ours left, waiting up to 5 s.
async fn ours_gone(fx: &PgFixture) -> bool {
    let ours = "SELECT count(*) FROM pg_stat_activity WHERE application_name = 'dbento' AND datname = current_database()";
    for _ in 0..50 {
        if fx.db.scalar(ours).await.as_deref() == Some("0") {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    false
}

fn about_ten_seconds(waited: Duration) {
    assert!(
        waited >= Duration::from_millis(9500) && waited < Duration::from_secs(15),
        "{waited:?}"
    );
}

#[tokio::test]
async fn a_drain_past_ten_seconds_closes_the_session() {
    bounded(async {
        let fx = pg_fixture("").await;
        let mut s = fx.session(SessionOptions::default()).await;
        let mut limits = Limits::default();
        limits.max_rows = Some(1);
        let started;
        {
            let mut script = s.script(&format!("{SLOW}; SELECT 'after'"));
            let ran = collect(script.next().await.unwrap().unwrap(), limits)
                .await
                .unwrap();
            assert_eq!((ran.outcome.rows, ran.outcome.truncated), (1, true));
            started = Instant::now();
            let next = script.next().await.unwrap();
            assert!(
                matches!(next, Err(ExecuteError::StoppedAtDrainCap)),
                "{:?}",
                next.err()
            );
        }
        about_ten_seconds(started.elapsed());
        let r = run(&mut s, "SELECT 1", Limits::default()).await;
        assert!(
            matches!(r[0], Err(ExecuteError::SessionClosed)),
            "{:?}",
            r[0].as_ref().err()
        );
        assert!(
            ours_gone(&fx).await,
            "the server still runs the session's query"
        );
    })
    .await;
}

#[tokio::test]
async fn a_dropped_read_only_stream_drains_under_the_same_cap() {
    bounded(async {
        let fx = pg_fixture("read_only = true").await;
        let mut s = fx.session(SessionOptions::default()).await;
        {
            let mut script = s.script(SLOW);
            let p = script.next().await.unwrap().unwrap();
            let mut stream = p.execute(Limits::default()).await.unwrap();
            assert!(stream.next_batch().await.unwrap().is_some());
        }
        let started = Instant::now();
        let r = run(&mut s, "SELECT 1", Limits::default()).await;
        assert!(
            matches!(r[0], Err(ExecuteError::SessionClosed)),
            "{:?}",
            r[0].as_ref().err()
        );
        about_ten_seconds(started.elapsed());
    })
    .await;
}

/// The two ways a session is read-only: the connection's setting, or forced as for AI queries.
fn read_only_ways() -> [(&'static str, SessionOptions); 2] {
    let mut forced = SessionOptions::default();
    forced.force_read_only = true;
    [
        ("read_only = true", SessionOptions::default()),
        ("", forced),
    ]
}

#[tokio::test]
async fn close_does_not_wait_for_unread_rows_in_a_read_only_session() {
    bounded(async {
        for (toml, opts) in read_only_ways() {
            let fx = pg_fixture(toml).await;
            let mut s = fx.session(opts).await;
            {
                let mut script = s.script(SLOW);
                let p = script.next().await.unwrap().unwrap();
                let mut stream = p.execute(Limits::default()).await.unwrap();
                assert!(stream.next_batch().await.unwrap().is_some());
            }
            tokio::time::timeout(Duration::from_secs(3), s.close())
                .await
                .expect("close waited for the rest of the result")
                .unwrap();
            assert!(
                ours_gone(&fx).await,
                "{toml}: the server still runs the closed session's query"
            );
        }
    })
    .await;
}

#[tokio::test]
async fn close_past_the_cap_in_a_read_write_session_reports_it_closed() {
    bounded(async {
        let fx = pg_fixture("").await;
        let mut s = fx.session(SessionOptions::default()).await;
        {
            let mut script = s.script(SLOW);
            let p = script.next().await.unwrap().unwrap();
            let mut stream = p.execute(Limits::default()).await.unwrap();
            assert!(stream.next_batch().await.unwrap().is_some());
        }
        let started = Instant::now();
        let closed = s.close().await;
        about_ten_seconds(started.elapsed());
        assert!(
            matches!(closed, Err(ExecuteError::StoppedAtDrainCap)),
            "{closed:?}"
        );
        assert!(
            ours_gone(&fx).await,
            "the server still runs the closed session's query"
        );
    })
    .await;
}

#[tokio::test]
async fn a_write_stopped_at_the_cap_is_rolled_back_even_when_its_rest_is_small() {
    bounded(async {
        let fx = pg_fixture("").await;
        fx.db
            .admin()
            .await
            .batch_execute(
                "CREATE TABLE w(a int);
                 CREATE FUNCTION ins(i int) RETURNS int LANGUAGE sql
                 AS 'INSERT INTO w VALUES (i) RETURNING i'",
            )
            .await
            .unwrap();
        let mut s = fx.session(SessionOptions::default()).await;
        let mut limits = Limits::default();
        limits.max_rows = Some(1);
        // The four large rows are flushed at once. Past the cap only one small row and the
        // commit are left, which a dropped connection alone would not stop: the server
        // notices the client is gone only when it flushes, after committing.
        let sql = "SELECT CASE WHEN g < 5 THEN repeat('x', 10000) \
                   ELSE pg_sleep(15)::text || ins(g)::text END FROM generate_series(1, 5) g";
        {
            let mut script = s.script(sql);
            let ran = collect(script.next().await.unwrap().unwrap(), limits)
                .await
                .unwrap();
            assert!(ran.outcome.truncated);
            let next = script.next().await.unwrap();
            assert!(
                matches!(next, Err(ExecuteError::StoppedAtDrainCap)),
                "{:?}",
                next.err()
            );
        }
        // Past the statement's own 15 s, so a write that ran on would be visible by now.
        tokio::time::sleep(Duration::from_secs(7)).await;
        assert!(ours_gone(&fx).await, "the stopped statement still runs");
        assert_eq!(
            fx.db.scalar("SELECT count(*) FROM w").await.as_deref(),
            Some("0")
        );
    })
    .await;
}

#[tokio::test]
async fn a_cancel_between_prepare_and_execute_is_a_cancel() {
    bounded(async {
        let fx = pg_fixture("").await;
        let mut s = fx.session(SessionOptions::default()).await;
        let cancel = s.cancel_handle();
        let mut script = s.script("SELECT pg_sleep(30)");
        let p = script.next().await.unwrap().unwrap();
        // The request needs its own connection, so it lands once the statement is running.
        cancel.cancel();
        let r = tokio::time::timeout(Duration::from_secs(20), collect(p, Limits::default()))
            .await
            .expect("the cancel did not stop pg_sleep");
        assert!(
            matches!(r, Err(ExecuteError::Cancelled)),
            "{:?}",
            r.as_ref().err()
        );
    })
    .await;
}

#[tokio::test]
async fn a_short_drain_keeps_the_open_transaction() {
    bounded(async {
        let fx = pg_fixture("").await;
        fx.db
            .admin()
            .await
            .batch_execute("CREATE TABLE t(a int)")
            .await
            .unwrap();
        let mut s = fx.session(SessionOptions::default()).await;
        let mut limits = Limits::default();
        limits.max_rows = Some(5);
        let sql = "BEGIN; INSERT INTO t VALUES (1); SELECT g FROM generate_series(1, 200000) g; SELECT count(*) FROM t; ROLLBACK";
        let r = run(&mut s, sql, limits).await;
        assert!(r[2].as_ref().unwrap().outcome.truncated);
        assert_eq!(r[3].as_ref().unwrap().rows, vec![vec![text("1")]]);
        assert!(r[4].is_ok());
        assert_eq!(
            fx.db.scalar("SELECT count(*) FROM t").await.as_deref(),
            Some("0")
        );
    })
    .await;
}

#[tokio::test]
async fn a_timeout_is_timeout_even_after_an_idle_cancel() {
    bounded(async {
        let fx = pg_fixture("statement_timeout = \"300ms\"").await;
        let mut s = fx.session(SessionOptions::default()).await;
        let r = run(&mut s, "SELECT pg_sleep(5)", Limits::default()).await;
        assert!(
            matches!(r[0], Err(ExecuteError::Timeout)),
            "{:?}",
            r[0].as_ref().err()
        );
        s.cancel_handle().cancel();
        tokio::time::sleep(Duration::from_millis(200)).await;
        let r = run(&mut s, "SELECT pg_sleep(5)", Limits::default()).await;
        assert!(
            matches!(r[0], Err(ExecuteError::Timeout)),
            "{:?}",
            r[0].as_ref().err()
        );
    })
    .await;
}

#[tokio::test]
async fn a_cancelled_read_only_statement_leaves_the_session_usable() {
    bounded(async {
        let fx = pg_fixture("read_only = true").await;
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
        let r = run(&mut s, "SHOW transaction_read_only", Limits::default()).await;
        assert_eq!(r[0].as_ref().unwrap().rows, vec![vec![text("on")]]);
    })
    .await;
}

/// About 3 s of 10 kB rows, each flushed as produced: short enough to drain under the cap.
const DRAINABLE: &str = "SELECT repeat('x', 10000), pg_sleep(0.01) FROM generate_series(1, 300)";

/// Runs `sql` truncated at one row as a whole script, and returns how long the end-of-script
/// call took.
async fn truncate_last(s: &mut dbento_core::Session, sql: &str) -> Duration {
    let mut limits = Limits::default();
    limits.max_rows = Some(1);
    let mut script = s.script(sql);
    let ran = collect(script.next().await.unwrap().unwrap(), limits)
        .await
        .unwrap();
    assert_eq!(
        (ran.outcome.rows, ran.outcome.truncated),
        (1, true),
        "one row, then truncated"
    );
    let started = Instant::now();
    assert!(
        script.next().await.is_none(),
        "an extra item after the last statement"
    );
    started.elapsed()
}

#[tokio::test]
async fn a_truncated_last_read_only_statement_is_not_drained_and_the_next_script_runs() {
    bounded(async {
        for (toml, opts) in read_only_ways() {
            let fx = pg_fixture(toml).await;
            let mut s = fx.session(opts).await;
            let waited = truncate_last(&mut s, DRAINABLE).await;
            assert!(waited < Duration::from_millis(500), "{toml}: {waited:?}");
            let r = run(&mut s, "SHOW transaction_read_only", Limits::default()).await;
            assert_eq!(r[0].as_ref().unwrap().rows, vec![vec![text("on")]], "{toml}");
            let state = "SELECT state FROM pg_stat_activity WHERE application_name = 'dbento' AND datname = current_database()";
            assert_eq!(fx.db.scalar(state).await.as_deref(), Some("idle"), "{toml}");
        }
    })
    .await;
}

#[tokio::test]
async fn a_truncated_last_statement_in_a_read_write_session_is_drained() {
    bounded(async {
        let fx = pg_fixture("").await;
        let mut s = fx.session(SessionOptions::default()).await;
        // DRAINABLE takes about 3 s, so the end of the script waited for it.
        let waited = truncate_last(&mut s, DRAINABLE).await;
        assert!(waited >= Duration::from_secs(2), "{waited:?}");
        let state = "SELECT state FROM pg_stat_activity WHERE application_name = 'dbento' AND datname = current_database()";
        assert_eq!(fx.db.scalar(state).await.as_deref(), Some("idle"));
        let r = run(&mut s, "SELECT 'after'", Limits::default()).await;
        assert_eq!(r[0].as_ref().unwrap().rows, vec![vec![text("after")]]);
        s.close().await.unwrap();
    })
    .await;
}

#[tokio::test]
async fn close_after_a_truncated_last_statement_stops_the_query() {
    bounded(async {
        for (toml, opts) in read_only_ways() {
            let fx = pg_fixture(toml).await;
            let mut s = fx.session(opts).await;
            let waited = truncate_last(&mut s, SLOW).await;
            assert!(waited < Duration::from_millis(500), "{toml}: {waited:?}");
            tokio::time::timeout(Duration::from_secs(3), s.close())
                .await
                .expect("close waited for the rest of the result")
                .unwrap();
            assert!(
                ours_gone(&fx).await,
                "{toml}: the server still runs the closed session's query"
            );
        }
    })
    .await;
}

#[tokio::test]
async fn a_truncated_last_write_commits_before_close() {
    bounded(async {
        // Ending the script drains the write; closing with the script dropped early does too.
        for end_script in [true, false] {
            let fx = pg_fixture("").await;
            fx.db
                .admin()
                .await
                .batch_execute(
                    "CREATE TABLE p(a int PRIMARY KEY); INSERT INTO p SELECT generate_series(1, 20000);
                     CREATE TABLE t(a int REFERENCES p DEFERRABLE INITIALLY DEFERRED, b text)",
                )
                .await
                .unwrap();
            let mut s = fx.session(SessionOptions::default()).await;
            let mut limits = Limits::default();
            limits.max_rows = Some(1);
            // More RETURNING rows than the socket buffers hold, and deferred key checks at
            // commit that notice a lost client, so an abort before the rows are read rolls back.
            let sql = "INSERT INTO t SELECT g, repeat('x', 2000) FROM generate_series(1, 20000) g RETURNING *";
            {
                let mut script = s.script(sql);
                let ran = collect(script.next().await.unwrap().unwrap(), limits)
                    .await
                    .unwrap();
                assert!(ran.outcome.truncated);
                if end_script {
                    assert!(script.next().await.is_none());
                }
            }
            s.close().await.unwrap();
            assert_eq!(
                fx.db.scalar("SELECT count(*) FROM t").await.as_deref(),
                Some("20000"),
                "end_script = {end_script}"
            );
        }
    })
    .await;
}

/// How a test leaves a truncated write before closing.
#[derive(Debug, Clone, Copy)]
enum Leave {
    /// Asks for the next statement, which drains the write.
    EndScript,
    /// Drops the script at once, so close drains the write.
    DropScript,
    /// Ends the script, then settles.
    Settle,
}

#[tokio::test]
async fn a_truncated_write_whose_commit_fails_reports_the_error() {
    bounded(async {
        for leave in [Leave::EndScript, Leave::DropScript, Leave::Settle] {
            let fx = pg_fixture("").await;
            fx.db
                .admin()
                .await
                .batch_execute(
                    "CREATE TABLE p(a int PRIMARY KEY);
                     CREATE TABLE t(a int REFERENCES p DEFERRABLE INITIALLY DEFERRED, b text)",
                )
                .await
                .unwrap();
            let mut s = fx.session(SessionOptions::default()).await;
            let mut limits = Limits::default();
            limits.max_rows = Some(1);
            // No parent row exists, so the deferred check fails the commit after every row.
            let sql = "INSERT INTO t SELECT g, repeat('x', 2000) FROM generate_series(1, 20000) g RETURNING *";
            {
                let mut script = s.script(sql);
                let ran = collect(script.next().await.unwrap().unwrap(), limits)
                    .await
                    .unwrap();
                assert!(ran.outcome.truncated);
                if !matches!(leave, Leave::DropScript) {
                    assert!(script.next().await.is_none(), "{leave:?}");
                }
            }
            let failure = if matches!(leave, Leave::Settle) {
                let settled = s.settle().await;
                s.close().await.expect("the settle already reported the error");
                settled
            } else {
                s.close().await
            };
            match failure {
                Err(ExecuteError::Database { code, .. }) => assert_eq!(code, "23503", "{leave:?}"),
                other => panic!("{leave:?}: {other:?}"),
            }
            assert_eq!(
                fx.db.scalar("SELECT count(*) FROM t").await.as_deref(),
                Some("0"),
                "{leave:?}"
            );
        }
    })
    .await;
}

/// A function the classifier sees only as a call: each row it returns also inserts into `t`,
/// whose deferred key check at commit notices a lost client.
const WRITING_FN: &str = "CREATE TABLE p(a int PRIMARY KEY);
     CREATE TABLE t(a int REFERENCES p DEFERRABLE INITIALLY DEFERRED, b text);
     CREATE FUNCTION f(g int) RETURNS text LANGUAGE sql AS
       $$ INSERT INTO t VALUES (g, repeat('x', 2000)) RETURNING b $$";
const CALLS_FN: &str = "SELECT f(g) FROM generate_series(1, 20000) g";

#[tokio::test]
async fn a_truncated_last_select_of_a_writing_function_commits_before_close() {
    bounded(async {
        for leave in [Leave::EndScript, Leave::DropScript] {
            let fx = pg_fixture("").await;
            let admin = fx.db.admin().await;
            admin.batch_execute(WRITING_FN).await.unwrap();
            admin
                .batch_execute("INSERT INTO p SELECT generate_series(1, 20000)")
                .await
                .unwrap();
            let mut s = fx.session(SessionOptions::default()).await;
            let mut limits = Limits::default();
            limits.max_rows = Some(1);
            {
                let mut script = s.script(CALLS_FN);
                let ran = collect(script.next().await.unwrap().unwrap(), limits)
                    .await
                    .unwrap();
                assert!(ran.outcome.truncated);
                if matches!(leave, Leave::EndScript) {
                    assert!(script.next().await.is_none());
                }
            }
            s.close().await.unwrap();
            assert_eq!(
                fx.db.scalar("SELECT count(*) FROM t").await.as_deref(),
                Some("20000"),
                "{leave:?}"
            );
        }
    })
    .await;
}

#[tokio::test]
async fn a_truncated_select_of_a_writing_function_whose_commit_fails_reports_the_error() {
    bounded(async {
        for leave in [Leave::EndScript, Leave::DropScript, Leave::Settle] {
            let fx = pg_fixture("").await;
            fx.db.admin().await.batch_execute(WRITING_FN).await.unwrap();
            let mut s = fx.session(SessionOptions::default()).await;
            let mut limits = Limits::default();
            limits.max_rows = Some(1);
            // No parent row exists, so the deferred check fails the commit after every row.
            {
                let mut script = s.script(CALLS_FN);
                let ran = collect(script.next().await.unwrap().unwrap(), limits)
                    .await
                    .unwrap();
                assert!(ran.outcome.truncated);
                if !matches!(leave, Leave::DropScript) {
                    assert!(script.next().await.is_none(), "{leave:?}");
                }
            }
            let failure = if matches!(leave, Leave::Settle) {
                let settled = s.settle().await;
                s.close()
                    .await
                    .expect("the settle already reported the error");
                settled
            } else {
                s.close().await
            };
            match failure {
                Err(ExecuteError::Database { code, .. }) => assert_eq!(code, "23503", "{leave:?}"),
                other => panic!("{leave:?}: {other:?}"),
            }
        }
    })
    .await;
}

/// Twenty 1 kB-row megabytes, far more than the socket buffers hold, so an unread rest
/// leaves the backend `active` and a drained one leaves it `idle`.
const BIG: &str =
    "CREATE TABLE big AS SELECT g AS a, repeat('x', 1000) AS b FROM generate_series(1, 20000) g";

/// The backend state of our one session.
async fn our_state(fx: &PgFixture) -> Option<String> {
    let state = "SELECT state FROM pg_stat_activity WHERE application_name = 'dbento' AND datname = current_database()";
    fx.db.scalar(state).await
}

/// Whether a truncated last `sql` in a read-write session was drained at the end of its
/// script, then closes the session.
async fn drained_at_end(fx: &PgFixture, sql: &str) -> bool {
    let mut s = fx.session(SessionOptions::default()).await;
    let waited = truncate_last(&mut s, sql).await;
    let state = our_state(fx).await;
    let drained = match state.as_deref() {
        Some("idle") => true,
        Some("active") => false,
        other => panic!("{sql}: state {other:?}"),
    };
    if !drained {
        assert!(waited < Duration::from_millis(500), "{sql}: {waited:?}");
    }
    tokio::time::timeout(Duration::from_secs(3), s.close())
        .await
        .expect("close waited for the rest of the result")
        .unwrap();
    assert!(
        ours_gone(fx).await,
        "{sql}: the server still runs the query"
    );
    drained
}

#[tokio::test]
async fn a_truncated_read_of_a_table_in_a_read_write_session_is_aborted() {
    bounded(async {
        let fx = pg_fixture("").await;
        fx.db.admin().await.batch_execute(BIG).await.unwrap();
        assert!(!drained_at_end(&fx, "SELECT * FROM big").await);
    })
    .await;
}

#[tokio::test]
async fn a_truncated_read_through_a_stable_function_is_aborted() {
    bounded(async {
        let fx = pg_fixture("").await;
        fx.db
            .admin()
            .await
            .batch_execute(&format!(
                "{BIG}; CREATE FUNCTION st(t text) RETURNS int STABLE LANGUAGE sql AS 'SELECT length(t)'"
            ))
            .await
            .unwrap();
        assert!(!drained_at_end(&fx, "SELECT st(b), b FROM big WHERE a BETWEEN 1 AND 20000").await);
    })
    .await;
}

#[tokio::test]
async fn a_truncated_read_of_a_view_over_a_writing_function_commits_before_close() {
    bounded(async {
        let fx = pg_fixture("").await;
        let admin = fx.db.admin().await;
        admin.batch_execute(WRITING_FN).await.unwrap();
        admin
            .batch_execute(&format!(
                "INSERT INTO p SELECT generate_series(1, 20000); CREATE VIEW v AS {CALLS_FN}"
            ))
            .await
            .unwrap();
        let mut s = fx.session(SessionOptions::default()).await;
        truncate_last(&mut s, "SELECT * FROM v").await;
        s.close().await.unwrap();
        assert_eq!(
            fx.db.scalar("SELECT count(*) FROM t").await.as_deref(),
            Some("20000")
        );
    })
    .await;
}

#[tokio::test]
async fn a_truncated_read_of_a_table_with_row_security_is_drained() {
    bounded(async {
        let fx = pg_fixture("").await;
        fx.db
            .admin()
            .await
            .batch_execute(&format!(
                "{BIG}; ALTER TABLE big ENABLE ROW LEVEL SECURITY; CREATE POLICY everyone ON big USING (true)"
            ))
            .await
            .unwrap();
        assert!(drained_at_end(&fx, "SELECT * FROM big").await);
    })
    .await;
}

/// Sets up `setup` in a fresh database, closes a read-write session after `sql` is cut at
/// one row, and returns `count` from what the close left committed.
async fn committed_after_close(setup: &str, sql: &str, count: &str) -> Option<String> {
    let fx = pg_fixture("").await;
    fx.db.admin().await.batch_execute(setup).await.unwrap();
    let mut s = fx.session(SessionOptions::default()).await;
    truncate_last(&mut s, sql).await;
    s.close().await.unwrap();
    fx.db.scalar(count).await
}

#[tokio::test]
async fn a_truncated_read_through_an_implicit_writing_cast_commits_before_close() {
    bounded(async {
        // length(int) resolves to length(text) through the implicit cast, which writes.
        let setup = format!(
            "{WRITING_FN}; INSERT INTO p SELECT generate_series(1, 20000);
             CREATE CAST (int AS text) WITH FUNCTION f(int) AS IMPLICIT"
        );
        let sql = "SELECT length(a), repeat('x', 2000) FROM p";
        let count = committed_after_close(&setup, sql, "SELECT count(*) FROM t").await;
        assert_eq!(count.as_deref(), Some("20000"));
    })
    .await;
}

/// A composite type whose default btree operator class logs every comparison it makes.
const LOGGING_OPCLASS: &str = "CREATE TABLE log(n int);
     CREATE TYPE v AS (a int);
     CREATE FUNCTION v_cmp(x v, y v) RETURNS int LANGUAGE sql AS $$ INSERT INTO log VALUES (1);
       SELECT CASE WHEN (x).a < (y).a THEN -1 WHEN (x).a > (y).a THEN 1 ELSE 0 END $$;
     CREATE FUNCTION v_lt(x v, y v) RETURNS bool LANGUAGE sql AS 'SELECT v_cmp(x, y) < 0';
     CREATE FUNCTION v_le(x v, y v) RETURNS bool LANGUAGE sql AS 'SELECT v_cmp(x, y) <= 0';
     CREATE FUNCTION v_eq(x v, y v) RETURNS bool LANGUAGE sql AS 'SELECT v_cmp(x, y) = 0';
     CREATE FUNCTION v_ge(x v, y v) RETURNS bool LANGUAGE sql AS 'SELECT v_cmp(x, y) >= 0';
     CREATE FUNCTION v_gt(x v, y v) RETURNS bool LANGUAGE sql AS 'SELECT v_cmp(x, y) > 0';
     CREATE OPERATOR < (FUNCTION = v_lt, LEFTARG = v, RIGHTARG = v);
     CREATE OPERATOR <= (FUNCTION = v_le, LEFTARG = v, RIGHTARG = v);
     CREATE OPERATOR = (FUNCTION = v_eq, LEFTARG = v, RIGHTARG = v);
     CREATE OPERATOR >= (FUNCTION = v_ge, LEFTARG = v, RIGHTARG = v);
     CREATE OPERATOR > (FUNCTION = v_gt, LEFTARG = v, RIGHTARG = v);
     CREATE OPERATOR CLASS v_ops DEFAULT FOR TYPE v USING btree AS
       OPERATOR 1 <, OPERATOR 2 <=, OPERATOR 3 =, OPERATOR 4 >=, OPERATOR 5 >,
       FUNCTION 1 v_cmp(v, v);
     CREATE TABLE vt(x v, y v);
     INSERT INTO vt VALUES (ROW(3), ROW(1)), (ROW(1), ROW(2)), (ROW(2), ROW(3))";

#[tokio::test]
async fn a_truncated_read_through_an_implied_writing_operator_commits_before_close() {
    bounded(async {
        // Each inner query compares v values without naming an operator; the cross join
        // with BIG keeps the server sending after the cut.
        for inner in [
            "SELECT x FROM vt ORDER BY x",
            "SELECT DISTINCT x FROM vt",
            "SELECT x FROM vt GROUP BY x",
            "SELECT x FROM vt JOIN vt u USING (x)",
            "SELECT GREATEST(x, y) FROM vt",
            "SELECT LEAST(x, y) FROM vt",
        ] {
            let setup = format!("{BIG}; {LOGGING_OPCLASS}");
            let sql = format!("SELECT b, w FROM big, ({inner} OFFSET 0) w");
            let count = committed_after_close(&setup, &sql, "SELECT count(*) > 0 FROM log").await;
            assert_eq!(count.as_deref(), Some("t"), "{inner}");
        }
    })
    .await;
}

#[tokio::test]
async fn a_truncated_read_through_an_attribute_notation_call_commits_before_close() {
    bounded(async {
        let setup = format!(
            "{WRITING_FN}; INSERT INTO p SELECT generate_series(1, 20000);
             CREATE FUNCTION fa(r p) RETURNS text LANGUAGE sql AS 'SELECT f((r).a)'"
        );
        for sql in ["SELECT p.fa FROM p", "SELECT (p).fa FROM p"] {
            let count = committed_after_close(&setup, sql, "SELECT count(*) FROM t").await;
            assert_eq!(count.as_deref(), Some("20000"), "{sql}");
        }
    })
    .await;
}

#[tokio::test]
async fn a_truncated_read_calling_a_writing_function_inside_an_array_commits_before_close() {
    bounded(async {
        let setup = format!("{WRITING_FN}; INSERT INTO p SELECT generate_series(1, 20000)");
        let sql = "SELECT ARRAY[f(a)] FROM p";
        let count = committed_after_close(&setup, sql, "SELECT count(*) FROM t").await;
        assert_eq!(count.as_deref(), Some("20000"));
    })
    .await;
}

#[tokio::test]
async fn a_truncated_read_coercing_into_a_domain_with_a_writing_check_commits_before_close() {
    bounded(async {
        // g names neither d nor w, but each call coerces its argument into d, whose CHECK writes.
        let setup = "CREATE TABLE log(n int); CREATE TABLE p(a int);
             INSERT INTO p SELECT generate_series(1, 20000);
             CREATE FUNCTION w(n int) RETURNS bool LANGUAGE plpgsql AS
               $$ BEGIN INSERT INTO log VALUES (n); RETURN true; END $$;
             CREATE DOMAIN d AS int CHECK (w(VALUE));
             CREATE FUNCTION g(x d) RETURNS int IMMUTABLE LANGUAGE sql AS 'SELECT x'";
        let sql = "SELECT g(a), repeat('x', 2000) FROM p";
        let count = committed_after_close(setup, sql, "SELECT count(*) FROM log").await;
        assert_eq!(count.as_deref(), Some("20000"));
    })
    .await;
}

/// Points `fx` at a new role that cannot read pg_proc, so the volatility check fails; the
/// database is the test's own. Returns the role, for `drop_role`.
async fn connect_without_pg_proc(fx: &mut PgFixture) -> String {
    let role = format!("noproc_{}", fx.db.database());
    fx.db
        .admin()
        .await
        .batch_execute(&format!(
            "{BIG}; DROP ROLE IF EXISTS \"{role}\";
             CREATE ROLE \"{role}\" LOGIN PASSWORD '{}'; GRANT SELECT ON big TO \"{role}\";
             REVOKE SELECT ON pg_catalog.pg_proc FROM PUBLIC",
            fx.db.password()
        ))
        .await
        .unwrap();
    let toml = std::fs::read_to_string(fx.dir.path().join("dbento.toml")).unwrap();
    let user = format!("user = \"{}\"", fx.db.user());
    assert!(toml.contains(&user), "{toml}");
    let toml = toml.replace(&user, &format!("user = \"{role}\""));
    std::fs::write(fx.dir.path().join("dbento.toml"), toml).unwrap();
    fx.ws = dbento_core::Workspace::open(fx.dir.path()).unwrap();
    role
}

async fn drop_role(fx: &PgFixture, role: &str) {
    fx.db
        .admin()
        .await
        .batch_execute(&format!(
            "REVOKE ALL ON big FROM \"{role}\"; DROP ROLE \"{role}\""
        ))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_truncated_read_is_drained_when_the_volatility_check_fails() {
    bounded(async {
        let mut fx = pg_fixture("").await;
        let role = connect_without_pg_proc(&mut fx).await;
        let drained = drained_at_end(&fx, "SELECT * FROM big").await;
        drop_role(&fx, &role).await;
        assert!(drained);
    })
    .await;
}

#[tokio::test]
async fn a_failing_check_never_aborts_the_users_transaction() {
    bounded(async {
        let mut fx = pg_fixture("").await;
        let role = connect_without_pg_proc(&mut fx).await;
        let mut s = fx.session(SessionOptions::default()).await;
        let mut limits = Limits::default();
        limits.max_rows = Some(1);
        let r = run(
            &mut s,
            "BEGIN; SELECT * FROM big; SELECT count(*) FROM big; COMMIT",
            limits,
        )
        .await;
        s.close().await.unwrap();
        drop_role(&fx, &role).await;
        let errors: Vec<_> = r.iter().filter_map(|x| x.as_ref().err()).collect();
        assert!(errors.is_empty(), "{errors:?}");
        assert_eq!(r.len(), 4);
        assert!(r[1].as_ref().unwrap().outcome.truncated);
        assert_eq!(r[2].as_ref().unwrap().rows, vec![vec![text("20000")]]);
    })
    .await;
}

#[tokio::test]
async fn close_after_a_truncated_read_inside_a_transaction_returns_promptly() {
    bounded(async {
        let fx = pg_fixture("").await;
        let mut s = fx.session(SessionOptions::default()).await;
        // SLOW calls pg_sleep, so outside a transaction it would drain to the cap.
        assert!(run(&mut s, "BEGIN", Limits::default()).await[0].is_ok());
        let waited = truncate_last(&mut s, SLOW).await;
        assert!(waited < Duration::from_millis(500), "{waited:?}");
        tokio::time::timeout(Duration::from_secs(3), s.close())
            .await
            .expect("close waited for the rest of the result")
            .unwrap();
        assert!(ours_gone(&fx).await, "the server still runs the query");
    })
    .await;
}

#[tokio::test]
async fn a_truncated_last_read_only_statement_ends_its_wrapper_with_the_script() {
    bounded(async {
        for (toml, opts) in read_only_ways() {
            let fx = pg_fixture(toml).await;
            let mut s = fx.session(opts).await;
            let waited = truncate_last(&mut s, SLOW).await;
            assert!(waited < Duration::from_millis(500), "{toml}: {waited:?}");
            // No further request: the end of the script alone has ended the transaction.
            assert_eq!(our_state(&fx).await.as_deref(), Some("idle"), "{toml}");
            let r = run(&mut s, "SELECT 'after'", Limits::default()).await;
            assert_eq!(
                r[0].as_ref().unwrap().rows,
                vec![vec![text("after")]],
                "{toml}"
            );
            s.close().await.unwrap();
        }
    })
    .await;
}

/// Leaves `sql` truncated at one row with its script dropped early, then drops the session
/// without closing it, and returns when that happened.
async fn drop_with_rows_unread(fx: &PgFixture, opts: SessionOptions, sql: &str) -> Instant {
    let mut s = fx.session(opts).await;
    {
        let mut script = s.script(sql);
        let p = script.next().await.unwrap().unwrap();
        let mut stream = p.execute(Limits::default()).await.unwrap();
        assert!(
            stream.next_batch().await.unwrap().is_some(),
            "a first batch arrives"
        );
    }
    drop(s);
    Instant::now()
}

#[tokio::test]
async fn a_read_only_session_dropped_with_rows_unread_stops_its_query() {
    bounded(async {
        for (toml, opts) in read_only_ways() {
            let fx = pg_fixture(toml).await;
            drop_with_rows_unread(&fx, opts, SLOW).await;
            assert!(
                ours_gone(&fx).await,
                "{toml}: the server still runs the dropped session's query"
            );
        }
    })
    .await;
}

#[tokio::test]
async fn a_read_write_session_dropped_with_rows_unread_stops_at_the_cap() {
    bounded(async {
        let fx = pg_fixture("").await;
        let dropped = drop_with_rows_unread(&fx, SessionOptions::default(), SLOW).await;
        // SLOW calls the volatile pg_sleep, so it may write and drains for the cap first.
        let mut gone = false;
        for _ in 0..3 {
            gone = ours_gone(&fx).await;
            if gone {
                break;
            }
        }
        assert!(gone, "the server still runs the dropped session's query");
        about_ten_seconds(dropped.elapsed());
    })
    .await;
}
