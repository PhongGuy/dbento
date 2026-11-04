mod support;
use async_channel::Receiver;
use dbento_app::runner::{
    AppLimits, Command, Event, RunnerSpec, RunnerTuning, StatementEnd, guarded, spawn_on,
    spawn_tuned,
};
use dbento_core::{Engine, ExecuteError, MemoryStore, RelationRef, Unguarded, Workspace};
use std::sync::Arc;
use std::time::{Duration, Instant};
use support::{LONG, MANY, fixture, my_spec, spec};

async fn next(rx: &Receiver<Event>) -> Event {
    tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("an event within 10 s")
        .expect("the runner is alive")
}
fn run(run_id: u64, sql: &str) -> Command {
    Command::Run {
        run_id,
        sql: sql.into(),
        limits: AppLimits {
            max_rows: Some(1_000),
            ..AppLimits::default()
        },
    }
}
fn here() -> tokio::runtime::Handle {
    tokio::runtime::Handle::current()
}

#[tokio::test(flavor = "multi_thread")]
async fn a_select_connects_lazily_and_streams() {
    let fx = fixture("", "");
    let r = spawn_on(&here(), spec(&fx, "lite"));
    let ev = r.events();
    r.send(run(1, "SELECT 1 AS a, 'x' AS b"));
    assert!(matches!(
        next(&ev).await,
        Event::Connected {
            read_only: false,
            engine: Engine::Sqlite,
            ..
        }
    ));
    assert!(
        matches!(next(&ev).await, Event::Executing { run_id: 1, stmt: 0, sql } if sql.contains("SELECT 1 AS a"))
    );
    match next(&ev).await {
        Event::StatementStarted {
            run_id: 1,
            stmt: 0,
            columns,
            ..
        } => {
            assert_eq!(
                columns.iter().map(|c| c.name()).collect::<Vec<_>>(),
                ["a", "b"]
            );
        }
        other => panic!("{other:?}"),
    }
    assert!(
        matches!(next(&ev).await, Event::Batch { run_id: 1, stmt: 0, batch } if batch.len() == 1)
    );
    match next(&ev).await {
        Event::StatementDone {
            run_id: 1,
            stmt: 0,
            end: StatementEnd::Complete(o),
            ..
        } => assert_eq!(o.rows, 1),
        other => panic!("{other:?}"),
    }
    assert!(matches!(next(&ev).await, Event::RunDone { run_id: 1 }));
}

#[tokio::test(flavor = "multi_thread")]
async fn the_row_cap_truncates() {
    let fx = fixture("", "");
    let tuning = RunnerTuning {
        park_at_cap: Duration::from_millis(200),
        ..RunnerTuning::default()
    };
    let r = spawn_tuned(&here(), spec(&fx, "lite"), tuning);
    let ev = r.events();
    r.send(run(1, MANY));
    let mut rows = 0;
    loop {
        match next(&ev).await {
            Event::Batch { batch, .. } => rows += batch.len(),
            Event::StatementDone {
                end: StatementEnd::Truncated(o),
                ..
            } => {
                assert_eq!(o.rows, 1000);
                break;
            }
            Event::StatementDone { end, .. } => panic!("{end:?}"),
            _ => {}
        }
    }
    assert_eq!(rows, 1000);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failure_ends_the_script() {
    let fx = fixture("", "");
    let r = spawn_on(&here(), spec(&fx, "lite"));
    let ev = r.events();
    r.send(run(1, "SELECT 1; SELEC x; SELECT 3"));
    let mut done = Vec::new();
    loop {
        match next(&ev).await {
            Event::StatementDone { stmt, end, .. } => done.push((stmt, end)),
            Event::RunDone { run_id: 1 } => break,
            _ => {}
        }
    }
    assert_eq!(done.len(), 2, "{done:?}");
    assert!(matches!(done[0], (0, StatementEnd::Complete(_))));
    assert!(
        matches!(&done[1], (1, StatementEnd::Failed(ExecuteError::Database { message, .. })) if message.contains("syntax error")),
        "{done:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn connect_failure_is_reported_and_retried() {
    let fx = fixture("", "");
    let r = spawn_on(&here(), spec(&fx, "nopg"));
    let ev = r.events();
    for id in [1, 2] {
        r.send(run(id, "SELECT 1"));
        match next(&ev).await {
            Event::ConnectFailed { message } => {
                assert!(message.starts_with("cannot open the database"), "{message}")
            }
            other => panic!("{other:?}"),
        }
        assert!(matches!(next(&ev).await, Event::RunDone { run_id } if run_id == id));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn close_and_drop_end_the_runner() {
    let fx = fixture("", "");
    let r = spawn_on(&here(), spec(&fx, "lite"));
    let ev = r.events();
    r.send(run(1, "SELECT 1"));
    while !matches!(next(&ev).await, Event::RunDone { .. }) {}
    r.send(Command::Close);
    assert!(
        tokio::time::timeout(Duration::from_secs(10), ev.recv())
            .await
            .unwrap()
            .is_err()
    );
    let r2 = spawn_on(&here(), spec(&fx, "lite"));
    let ev2 = r2.events();
    drop(r2);
    assert!(
        tokio::time::timeout(Duration::from_secs(10), ev2.recv())
            .await
            .unwrap()
            .is_err()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_panic_becomes_stopped() {
    let (tx, rx) = async_channel::unbounded();
    guarded(
        async { panic!("boom") },
        Arc::new(tokio::sync::Mutex::new(None)),
        tx,
    )
    .await;
    assert!(
        matches!(rx.recv().await.unwrap(), Event::Stopped { message } if message == "session stopped")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn cancel_stops_a_long_statement() {
    let fx = fixture("", "");
    let r = spawn_on(&here(), spec(&fx, "lite"));
    let ev = r.events();
    r.send(run(1, LONG));
    let Event::Connected { cancel, .. } = next(&ev).await else {
        panic!("not connected")
    };
    // SQLite answers `execute` only after the first step, which here never ends.
    assert!(matches!(next(&ev).await, Event::Executing { run_id: 1, stmt: 0, sql } if sql == LONG));
    tokio::time::sleep(Duration::from_millis(300)).await;
    cancel.cancel();
    loop {
        match next(&ev).await {
            Event::StatementDone {
                end: StatementEnd::Failed(ExecuteError::Cancelled),
                ..
            } => break,
            Event::StatementDone { end, .. } => panic!("{end:?}"),
            _ => {}
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn postgres_streams_and_cancels_pg_sleep_within_a_second() {
    let db = dbento_testkit::pg_db().await;
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("dbento.toml"),
        format!("dbento = 1\n{}", db.connection_toml("pg", "")),
    )
    .unwrap();
    let password = db.password().to_owned();
    let spec = RunnerSpec {
        workspace: Arc::new(Workspace::open(dir.path()).unwrap()),
        connection: "pg".into(),
        store: Arc::new(MemoryStore::new()),
        env: Arc::new(move |v| (v == dbento_testkit::SECRET_VAR).then(|| password.clone())),
    };
    let r = spawn_on(&here(), spec);
    let ev = r.events();
    r.send(run(1, "SELECT 1 AS a"));
    let Event::Connected {
        engine: Engine::Postgres,
        cancel,
        ..
    } = next(&ev).await
    else {
        panic!("not connected")
    };
    while !matches!(next(&ev).await, Event::RunDone { run_id: 1 }) {}
    r.send(run(2, "SELECT pg_sleep(60)"));
    tokio::time::sleep(Duration::from_millis(300)).await;
    let asked = Instant::now();
    cancel.cancel();
    loop {
        if let Event::StatementDone { run_id: 2, end, .. } = next(&ev).await {
            assert!(
                matches!(end, StatementEnd::Failed(ExecuteError::Cancelled)),
                "{end:?}"
            );
            break;
        }
    }
    assert!(
        asked.elapsed() < Duration::from_secs(1),
        "{:?}",
        asked.elapsed()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn unguarded_update_waits_and_skip_runs_nothing() {
    let fx = fixture("CREATE TABLE t(x); INSERT INTO t VALUES (1),(2),(3);", "");
    let r = spawn_on(&here(), spec(&fx, "lite"));
    let ev = r.events();
    r.send(run(1, "UPDATE t SET x = 0"));
    next(&ev).await; // Connected
    match next(&ev).await {
        Event::NeedsConfirm {
            run_id: 1,
            stmt: 0,
            kinds,
            sql,
            ..
        } => {
            assert_eq!(kinds, [Unguarded::UpdateWithoutWhere]);
            assert!(sql.contains("UPDATE t"), "{sql}");
        }
        other => panic!("{other:?}"),
    }
    r.send(Command::Confirm { run_id: 0, stmt: 0 }); // stale: ignored
    r.send(Command::Skip { run_id: 1, stmt: 0 });
    assert!(matches!(
        next(&ev).await,
        Event::StatementDone {
            end: StatementEnd::Skipped,
            ..
        }
    ));
    assert!(matches!(next(&ev).await, Event::RunDone { run_id: 1 }));
    assert_eq!(fx.count("SELECT count(*) FROM t WHERE x = 0"), 0);
    r.send(run(2, "UPDATE t SET x = 0"));
    assert!(matches!(
        next(&ev).await,
        Event::NeedsConfirm { run_id: 2, .. }
    ));
    r.send(Command::Confirm { run_id: 2, stmt: 0 });
    assert!(matches!(
        next(&ev).await,
        Event::Executing { run_id: 2, .. }
    ));
    assert!(matches!(
        next(&ev).await,
        Event::StatementStarted { run_id: 2, .. }
    ));
    match next(&ev).await {
        Event::StatementDone {
            end: StatementEnd::Complete(o),
            ..
        } => assert_eq!(o.rows_affected, Some(3)),
        other => panic!("{other:?}"),
    }
    assert_eq!(fx.count("SELECT count(*) FROM t WHERE x = 0"), 3);
}

#[tokio::test(flavor = "multi_thread")]
async fn read_only_refuses_without_asking() {
    let fx = fixture(
        "CREATE TABLE t(x); INSERT INTO t VALUES (1);",
        "read_only = true",
    );
    let r = spawn_on(&here(), spec(&fx, "lite"));
    let ev = r.events();
    r.send(run(1, "UPDATE t SET x = 0"));
    assert!(matches!(
        next(&ev).await,
        Event::Connected {
            read_only: true,
            ..
        }
    ));
    loop {
        match next(&ev).await {
            Event::StatementDone {
                end: StatementEnd::Failed(ExecuteError::ReadOnlyViolation { .. }),
                ..
            } => break,
            Event::NeedsConfirm { .. } => panic!("asked on a read-only session"),
            Event::StatementDone { end, .. } => panic!("{end:?}"),
            _ => {}
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_run_abandons_a_waiting_statement() {
    let fx = fixture("CREATE TABLE t(x); INSERT INTO t VALUES (1);", "");
    let r = spawn_on(&here(), spec(&fx, "lite"));
    let ev = r.events();
    r.send(run(1, "DELETE FROM t"));
    next(&ev).await; // Connected
    assert!(
        matches!(next(&ev).await, Event::NeedsConfirm { run_id: 1, ref kinds, .. } if kinds == &[Unguarded::DeleteWithoutWhere])
    );
    r.send(run(2, "SELECT count(*) FROM t"));
    assert!(matches!(next(&ev).await, Event::RunDone { run_id: 1 }));
    assert!(matches!(
        next(&ev).await,
        Event::Executing { run_id: 2, .. }
    ));
    assert!(matches!(
        next(&ev).await,
        Event::StatementStarted { run_id: 2, .. }
    ));
    match next(&ev).await {
        Event::Batch {
            run_id: 2, batch, ..
        } => assert_eq!(batch.cell(0, 0), dbento_core::CellRef::Text("1")),
        other => panic!("{other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_keychain_secret_adds_the_hint() {
    let fx = fixture("", "");
    let r = spawn_on(&here(), spec(&fx, "nopg"));
    let ev = r.events();
    r.send(run(1, "SELECT 1"));
    match next(&ev).await {
        Event::ConnectFailed { message } => {
            assert!(message.starts_with("cannot open the database"), "{message}");
            assert!(
                message.contains("\nhint: no secret was found for 'nopg'"),
                "{message}"
            );
            assert!(message.contains("DBENTO_SECRET_NOPG"), "{message}");
        }
        other => panic!("{other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_statement_that_fails_to_prepare_takes_no_time() {
    let fx = fixture("", "");
    let r = spawn_on(&here(), spec(&fx, "lite"));
    let ev = r.events();
    r.send(run(1, "SELEC x"));
    loop {
        if let Event::StatementDone { end, elapsed, .. } = next(&ev).await {
            assert!(matches!(end, StatementEnd::Failed(_)), "{end:?}");
            assert_eq!(elapsed, Duration::ZERO);
            break;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_wait_for_an_answer_is_not_statement_time() {
    let fx = fixture("CREATE TABLE t(x); INSERT INTO t VALUES (1);", "");
    let r = spawn_on(&here(), spec(&fx, "lite"));
    let ev = r.events();
    r.send(run(1, "UPDATE t SET x = 0"));
    while !matches!(next(&ev).await, Event::NeedsConfirm { .. }) {}
    tokio::time::sleep(Duration::from_millis(300)).await;
    r.send(Command::Confirm { run_id: 1, stmt: 0 });
    loop {
        if let Event::StatementDone { elapsed, .. } = next(&ev).await {
            assert!(elapsed < Duration::from_millis(300), "{elapsed:?}");
            break;
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn close_while_asking_ends_the_runner_without_running() {
    let fx = fixture("CREATE TABLE t(x); INSERT INTO t VALUES (1);", "");
    let r = spawn_on(&here(), spec(&fx, "lite"));
    let ev = r.events();
    r.send(run(1, "UPDATE t SET x = 0"));
    while !matches!(next(&ev).await, Event::NeedsConfirm { .. }) {}
    r.send(Command::Close);
    loop {
        match tokio::time::timeout(Duration::from_secs(10), ev.recv()).await {
            Ok(Err(_)) => break,
            Ok(Ok(Event::StatementDone { end, .. })) => panic!("{end:?}"),
            Ok(Ok(_)) => {}
            Err(_) => panic!("the runner did not end"),
        }
    }
    assert_eq!(fx.count("SELECT count(*) FROM t WHERE x = 0"), 0);
}

#[tokio::test(flavor = "multi_thread")]
// The session's lock is gone once the runner has ended. A mere drop also releases it within
// microseconds on SQLite's worker thread, so this cannot tell Close from drop.
async fn close_releases_the_session_before_the_runner_ends() {
    let fx = fixture("CREATE TABLE t(x);", "");
    let r = spawn_on(&here(), spec(&fx, "lite"));
    let ev = r.events();
    r.send(run(1, "BEGIN EXCLUSIVE"));
    while !matches!(next(&ev).await, Event::RunDone { .. }) {}
    r.send(Command::Close);
    while tokio::time::timeout(Duration::from_secs(10), ev.recv())
        .await
        .expect("the runner ends")
        .is_ok()
    {}
    let other = rusqlite::Connection::open(fx.dir.path().join("data/app.db")).unwrap();
    other.busy_timeout(Duration::ZERO).unwrap();
    other.execute_batch("BEGIN IMMEDIATE; COMMIT").unwrap();
}

/// Three statements; the middle one runs long enough for a command to land while it does.
const SLOW_MIDDLE: &str = "INSERT INTO t VALUES (1); \
    WITH RECURSIVE r(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM r WHERE x < 1000000) SELECT count(*) FROM r; \
    INSERT INTO t VALUES (2)";

#[tokio::test(flavor = "multi_thread")]
async fn close_mid_script_runs_no_further_statement() {
    let fx = fixture("CREATE TABLE t(x);", "");
    let r = spawn_on(&here(), spec(&fx, "lite"));
    let ev = r.events();
    r.send(run(1, SLOW_MIDDLE));
    while !matches!(next(&ev).await, Event::StatementDone { stmt: 0, .. }) {}
    r.send(Command::Close);
    while tokio::time::timeout(Duration::from_secs(10), ev.recv())
        .await
        .expect("the runner ends")
        .is_ok()
    {}
    assert_eq!(fx.count("SELECT count(*) FROM t"), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_run_mid_script_runs_no_further_statement() {
    let fx = fixture("CREATE TABLE t(x);", "");
    let r = spawn_on(&here(), spec(&fx, "lite"));
    let ev = r.events();
    r.send(run(1, SLOW_MIDDLE));
    while !matches!(next(&ev).await, Event::StatementDone { stmt: 0, .. }) {}
    r.send(run(2, "SELECT count(*) FROM t"));
    loop {
        match next(&ev).await {
            Event::StatementDone {
                run_id: 1, stmt: 2, ..
            } => panic!("statement 2 ran"),
            Event::RunDone { run_id: 1 } => break,
            _ => {}
        }
    }
    loop {
        match next(&ev).await {
            Event::Batch {
                run_id: 2, batch, ..
            } => {
                assert_eq!(batch.cell(0, 0), dbento_core::CellRef::Text("1"));
                break;
            }
            Event::RunDone { .. } => panic!("run 2 returned no rows"),
            _ => {}
        }
    }
}

async fn until_run_done(ev: &Receiver<Event>, id: u64) -> Vec<Event> {
    let mut seen = Vec::new();
    loop {
        let e = next(ev).await;
        let done = matches!(e, Event::RunDone { run_id } if run_id == id);
        seen.push(e);
        if done {
            return seen;
        }
    }
}
async fn until_asked(ev: &Receiver<Event>) {
    while !matches!(next(ev).await, Event::NeedsConfirm { .. }) {}
}

#[tokio::test(flavor = "multi_thread")]
async fn stop_while_asking_skips_the_statement_and_ends_the_script() {
    let fx = fixture("CREATE TABLE t(x); INSERT INTO t VALUES (1);", "");
    let r = spawn_on(&here(), spec(&fx, "lite"));
    let ev = r.events();
    r.send(run(1, "UPDATE t SET x = 0; SELECT 5"));
    until_asked(&ev).await;
    r.send(Command::Stop { run_id: 1 });
    let seen = until_run_done(&ev, 1).await;
    assert!(
        seen.iter().any(|e| matches!(
            e,
            Event::StatementDone {
                stmt: 0,
                end: StatementEnd::Skipped,
                ..
            }
        )),
        "{seen:?}"
    );
    assert!(
        !seen
            .iter()
            .any(|e| matches!(e, Event::Executing { stmt: 1, .. })),
        "{seen:?}"
    );
    assert_eq!(fx.count("SELECT count(*) FROM t WHERE x = 0"), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stale_stop_while_asking_leaves_the_question() {
    let fx = fixture("CREATE TABLE t(x); INSERT INTO t VALUES (1);", "");
    let r = spawn_on(&here(), spec(&fx, "lite"));
    let ev = r.events();
    r.send(run(1, "UPDATE t SET x = 0"));
    until_asked(&ev).await;
    r.send(Command::Stop { run_id: 0 });
    r.send(Command::Confirm { run_id: 1, stmt: 0 });
    until_run_done(&ev, 1).await;
    assert_eq!(fx.count("SELECT count(*) FROM t WHERE x = 0"), 1);
}

fn ends(events: Vec<Event>) -> Vec<StatementEnd> {
    events
        .into_iter()
        .filter_map(|e| match e {
            Event::StatementDone { end, .. } => Some(end),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn mysql_runs_asks_cancels_and_refuses_editing() {
    for (flavor, db) in dbento_testkit::my_matrix().await {
        let (_dir, spec) = my_spec(&db, "");
        let r = spawn_on(&here(), spec);
        let ev = r.events();
        r.send(run(
            1,
            "CREATE TABLE t (x INT); INSERT INTO t VALUES (1), (2)",
        ));
        let Event::Connected {
            engine: Engine::Mysql,
            read_only: false,
            cancel,
            ..
        } = next(&ev).await
        else {
            panic!("{flavor}: not connected")
        };
        let done = ends(until_run_done(&ev, 1).await);
        assert!(
            matches!(
                done.as_slice(),
                [StatementEnd::Complete(_), StatementEnd::Complete(_)]
            ),
            "{flavor}: {done:?}"
        );

        r.send(run(2, "UPDATE t SET x = 0"));
        match next(&ev).await {
            Event::NeedsConfirm {
                run_id: 2, kinds, ..
            } => assert_eq!(kinds, [Unguarded::UpdateWithoutWhere], "{flavor}"),
            other => panic!("{flavor}: {other:?}"),
        }
        r.send(Command::Skip { run_id: 2, stmt: 0 });
        until_run_done(&ev, 2).await;
        assert_eq!(
            db.scalar("SELECT COUNT(*) FROM t WHERE x = 0")
                .await
                .as_deref(),
            Some("0"),
            "{flavor}"
        );

        r.send(run(3, "SELECT SLEEP(60)"));
        tokio::time::sleep(Duration::from_millis(300)).await;
        let asked = Instant::now();
        cancel.cancel();
        let done = ends(until_run_done(&ev, 3).await);
        assert!(
            matches!(
                done.as_slice(),
                [StatementEnd::Failed(ExecuteError::Cancelled)]
            ),
            "{flavor}: {done:?}"
        );
        assert!(asked.elapsed() < Duration::from_secs(2), "{flavor}");

        r.send(Command::Target {
            rel: RelationRef::new(db.database(), "t"),
        });
        loop {
            if let Event::Target { result, .. } = next(&ev).await {
                assert_eq!(
                    result.err().as_deref(),
                    Some("editing rows is not available on mysql yet"),
                    "{flavor}"
                );
                break;
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn mysql_read_only_refuses_without_asking_and_a_timeout_stops_the_statement() {
    for (flavor, db) in dbento_testkit::my_matrix().await {
        let (_rw_dir, rw) = my_spec(&db, "");
        let rw = spawn_on(&here(), rw);
        let rw_ev = rw.events();
        rw.send(run(1, "CREATE TABLE t (x INT)"));
        until_run_done(&rw_ev, 1).await;

        let (_dir, spec) = my_spec(&db, "read_only = true\nstatement_timeout = \"500ms\"");
        let r = spawn_on(&here(), spec);
        let ev = r.events();
        r.send(run(1, "DELETE FROM t"));
        assert!(
            matches!(
                next(&ev).await,
                Event::Connected {
                    read_only: true,
                    ..
                }
            ),
            "{flavor}"
        );
        let seen = until_run_done(&ev, 1).await;
        assert!(
            !seen.iter().any(|e| matches!(e, Event::NeedsConfirm { .. })),
            "{flavor}: asked on a read-only session"
        );
        let done = ends(seen);
        assert!(
            matches!(
                done.as_slice(),
                [StatementEnd::Failed(ExecuteError::ReadOnlyViolation { .. })]
            ),
            "{flavor}: {done:?}"
        );

        r.send(run(2, "SELECT SLEEP(5)"));
        let started = Instant::now();
        let done = ends(until_run_done(&ev, 2).await);
        assert!(
            matches!(
                done.as_slice(),
                [StatementEnd::Failed(ExecuteError::Timeout)]
            ),
            "{flavor}: {done:?}"
        );
        assert!(started.elapsed() < Duration::from_secs(3), "{flavor}");
    }
}
