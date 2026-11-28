//! A Stop ends its run whatever moment it lands, including while a prepare's settle spends
//! the cancel fired with it.
use crate::support::{
    fixture, here, ms_spec, my_spec, next_event, pg_spec, run_cmd, spec, until_event,
};
use std::time::Duration;

use dbento_app::runner::{
    Command, Event, RunnerSpec, RunnerTuning, StatementEnd, StopReason, spawn_on, spawn_tuned,
};
use dbento_core::{Engine, ExplainMode, explain_request};

/// Runs `sql` with a one-row cap, so its first statement is cut and its slow leftover is
/// drained by the next statement's prepare. On that drain's `Settling` it sends Stop and then
/// fires the cancel, as the tab does, and waits at most 10 s for `RunDone`: the second
/// statement sleeps for 30. A later `SELECT 1` must then run whole.
async fn stop_while_the_next_statement_prepares(spec: RunnerSpec, sql: &str) {
    let r = spawn_on(&here(), spec);
    let ev = r.events();
    r.send(run_cmd(1, sql, Some(1)));
    let Event::Connected { cancel, .. } = next_event(&ev).await else {
        panic!("the runner did not connect first");
    };
    let (mut seen, _) = until_event(&ev, |e| matches!(e, Event::Settling { run_id: 1 })).await;
    r.send(Command::Stop { run_id: 1 });
    cancel.cancel();
    let (rest, _) = tokio::time::timeout(
        Duration::from_secs(10),
        until_event(&ev, |e| matches!(e, Event::RunDone { run_id: 1 })),
    )
    .await
    .expect("the Stop was lost and the next statement ran");
    seen.extend(rest);
    assert!(
        !seen.iter().any(|e| matches!(
            e,
            Event::Executing {
                run_id: 1,
                stmt: 1,
                ..
            }
        )),
        "{seen:?}"
    );
    r.send(run_cmd(2, "SELECT 1", None));
    let (next, _) = until_event(&ev, |e| matches!(e, Event::RunDone { run_id: 2 })).await;
    assert!(complete(&next, 2), "{next:?}");
}

/// Whether statement 0 of run `run_id` ended complete.
fn complete(seen: &[Event], run_id: u64) -> bool {
    seen.iter().any(|e| {
        matches!(e, Event::StatementDone { run_id: r, stmt: 0, end: StatementEnd::Complete(_), .. } if *r == run_id)
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn a_postgres_stop_while_the_next_statement_prepares_ends_the_script() {
    // Each row outgrows the server's send buffer, so it is sent as soon as its sleep ends.
    let (_db, _dir, spec) = pg_spec("read_only = true").await;
    stop_while_the_next_statement_prepares(
        spec,
        "SELECT pg_sleep(0.2), repeat('x', 20000) FROM generate_series(1, 100); SELECT pg_sleep(30)",
    )
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_mysql_stop_while_the_next_statement_prepares_ends_the_script() {
    // Each row outgrows the server's net buffer, so it is sent as soon as its SLEEP ends.
    let sql = "WITH RECURSIVE r(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM r WHERE x < 100) \
               SELECT SLEEP(0.2), REPEAT('x', 20000) FROM r; SELECT SLEEP(30)";
    for (label, db) in dbento_testkit::my_matrix().await {
        let (_dir, spec) = my_spec(&db, "read_only = true");
        eprintln!("{label}");
        stop_while_the_next_statement_prepares(spec, sql).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sql_server_stop_while_the_next_statement_prepares_ends_the_script() {
    // T-SQL has no per-row sleep, so the leftover is slow by size: billions of rows.
    dbento_testkit::skip_without_mssql!();
    let db = dbento_testkit::mssql_db().await;
    let (_dir, spec) = ms_spec(&db, "tls = \"off\"");
    stop_while_the_next_statement_prepares(
        spec,
        "SELECT a.object_id, REPLICATE('x', 2000) FROM sys.all_objects a \
         CROSS JOIN sys.all_objects b CROSS JOIN sys.all_objects c\nGO\nWAITFOR DELAY '00:00:30'",
    )
    .await;
}

/// Stop and then the cancel, sent after run 1 ended, as a tab whose Stop came too late does;
/// the cancel lands on an idle session. The next run's `next_sql` must still end complete.
async fn stop_and_cancel_when_idle_then_run(spec: RunnerSpec, next_sql: &str) {
    let r = spawn_on(&here(), spec);
    let ev = r.events();
    r.send(run_cmd(1, "SELECT 1", None));
    let Event::Connected { cancel, .. } = next_event(&ev).await else {
        panic!("the runner did not connect first");
    };
    until_event(&ev, |e| matches!(e, Event::RunDone { run_id: 1 })).await;
    r.send(Command::Stop { run_id: 1 });
    cancel.cancel();
    r.send(run_cmd(2, next_sql, None));
    let (seen, _) = tokio::time::timeout(
        Duration::from_secs(10),
        until_event(&ev, |e| matches!(e, Event::RunDone { run_id: 2 })),
    )
    .await
    .expect("the next run never ended");
    assert!(complete(&seen, 2), "{seen:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_postgres_idle_stop_and_cancel_leave_the_next_run_whole() {
    let (_db, _dir, spec) = pg_spec("").await;
    stop_and_cancel_when_idle_then_run(spec, "SELECT pg_sleep(0.5), 1").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_mysql_idle_stop_and_cancel_leave_the_next_run_whole() {
    for (label, db) in dbento_testkit::my_matrix().await {
        let (_dir, spec) = my_spec(&db, "");
        eprintln!("{label}");
        stop_and_cancel_when_idle_then_run(spec, "SELECT SLEEP(0.5), 1").await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sqlite_idle_stop_and_cancel_leave_the_next_run_whole() {
    let fx = fixture("CREATE TABLE t(x);", "");
    stop_and_cancel_when_idle_then_run(
        spec(&fx, "lite"),
        "WITH RECURSIVE r(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM r WHERE x < 2000000) \
         SELECT count(*) FROM r",
    )
    .await;
}

#[cfg(feature = "duckdb")]
#[tokio::test(flavor = "multi_thread")]
async fn a_duckdb_idle_stop_and_cancel_leave_the_next_run_whole() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::create_dir(dir.path().join("data")).unwrap();
    dbento_testkit::duckdb_file(
        &dir.path().join("data/app.duckdb"),
        "CREATE TABLE t(x INT);",
    );
    std::fs::write(
        dir.path().join("dbento.toml"),
        "dbento = 1\n[connections.duck]\nengine = \"duckdb\"\npath = \"data/app.duckdb\"\n",
    )
    .unwrap();
    let spec = RunnerSpec {
        workspace: std::sync::Arc::new(dbento_core::Workspace::open(dir.path()).unwrap()),
        connection: "duck".into(),
        store: std::sync::Arc::new(dbento_core::MemoryStore::new()),
        env: std::sync::Arc::new(|_| None),
        ssh: dbento_app::ssh::SshContext::none(),
    };
    stop_and_cancel_when_idle_then_run(spec, "SELECT count(*) FROM range(50000000)").await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sql_server_idle_stop_and_cancel_leave_the_next_run_whole() {
    dbento_testkit::skip_without_mssql!();
    let db = dbento_testkit::mssql_db().await;
    let (_dir, spec) = ms_spec(&db, "tls = \"off\"");
    stop_and_cancel_when_idle_then_run(spec, "SELECT 1").await;
}

/// Queues `Explain{2}` and its `Stop` behind a run parked on a full event channel, so the
/// Stop is waiting before the explain starts. Asserts the explain is skipped, not planned.
async fn a_queued_stop_skips_the_explain(spec: RunnerSpec, engine: Engine) {
    let tuning = RunnerTuning {
        events_capacity: 1,
        ..RunnerTuning::default()
    };
    let r = spawn_tuned(&here(), spec, tuning);
    let ev = r.events();
    r.send(Command::Connect);
    while ev.is_empty() {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    // Connected fills the channel, so run 1 parks on its first event and reads nothing more.
    r.send(run_cmd(1, "SELECT 1", None));
    tokio::time::sleep(Duration::from_millis(300)).await;
    r.send(Command::Explain {
        run_id: 2,
        request: explain_request(engine, "SELECT 1", ExplainMode::Plan).unwrap(),
    });
    r.send(Command::Stop { run_id: 2 });
    let (seen, _) = tokio::time::timeout(
        Duration::from_secs(10),
        until_event(&ev, |e| matches!(e, Event::RunDone { run_id: 2 })),
    )
    .await
    .expect("the explain's run never ended");
    assert!(
        !seen.iter().any(|e| matches!(
            e,
            Event::Planned {
                run_id: 2,
                result: Ok(_),
                ..
            }
        )),
        "{seen:?}"
    );
    assert!(
        seen.iter().any(|e| matches!(
            e,
            Event::StatementDone {
                run_id: 2,
                stmt: 0,
                end: StatementEnd::Skipped,
                ..
            }
        )),
        "{seen:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_queued_stop_skips_a_sqlite_explain() {
    let fx = fixture("CREATE TABLE t(x);", "");
    a_queued_stop_skips_the_explain(spec(&fx, "lite"), Engine::Sqlite).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_queued_stop_skips_a_postgres_explain() {
    let (_db, _dir, spec) = pg_spec("read_only = true").await;
    a_queued_stop_skips_the_explain(spec, Engine::Postgres).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_queued_stop_skips_a_mysql_explain() {
    for (label, db) in dbento_testkit::my_matrix().await {
        let (_dir, spec) = my_spec(&db, "");
        eprintln!("{label}");
        a_queued_stop_skips_the_explain(spec, Engine::Mysql).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
// The tab sends Stop and then the cancel; when the Stop cuts the stream first, the cancel
// lands on the drain that settles the cut, which must not read as a failed statement.
async fn a_cancel_on_the_drain_after_a_stop_settles_clean() {
    let (_db, _dir, spec) = pg_spec("").await;
    let r = spawn_on(&here(), spec);
    let ev = r.events();
    r.send(run_cmd(
        1,
        "SELECT pg_sleep(0.05), repeat('x', 20000) FROM generate_series(1, 400); SELECT 1",
        None,
    ));
    let Event::Connected { cancel, .. } = next_event(&ev).await else {
        panic!("the runner did not connect first");
    };
    let (mut seen, _) = until_event(&ev, |e| matches!(e, Event::Batch { run_id: 1, .. })).await;
    r.send(Command::Stop { run_id: 1 });
    let (more, _) = until_event(&ev, |e| matches!(e, Event::Settling { run_id: 1 })).await;
    seen.extend(more);
    cancel.cancel();
    let (rest, _) = tokio::time::timeout(
        Duration::from_secs(10),
        until_event(&ev, |e| matches!(e, Event::RunDone { run_id: 1 })),
    )
    .await
    .expect("the drain outlived the cancel");
    seen.extend(rest);
    assert!(
        seen.iter().any(|e| matches!(
            e,
            Event::StatementDone {
                run_id: 1,
                stmt: 0,
                end: StatementEnd::Stopped {
                    reason: StopReason::User,
                    ..
                },
                ..
            }
        )),
        "{seen:?}"
    );
    assert!(
        !seen.iter().any(|e| matches!(
            e,
            Event::SettleFailed { .. }
                | Event::Disconnected { .. }
                | Event::Executing { stmt: 1, .. }
        )),
        "{seen:?}"
    );
}

#[tokio::test(flavor = "multi_thread")]
// Only a cut's own drain is excused: a cancel that fails a drain the next statement's prepare
// ran, with no Stop, still reaches the run's end as a failed settle.
async fn a_cancelled_drain_before_a_clean_end_still_reports() {
    let (_db, _dir, spec) = pg_spec("").await;
    let r = spawn_on(&here(), spec);
    let ev = r.events();
    r.send(run_cmd(
        1,
        "SELECT pg_sleep(0.05), repeat('x', 20000) FROM generate_series(1, 400); SELECT 1",
        Some(1),
    ));
    let Event::Connected { cancel, .. } = next_event(&ev).await else {
        panic!("the runner did not connect first");
    };
    let (mut seen, _) = until_event(&ev, |e| matches!(e, Event::Settling { run_id: 1 })).await;
    cancel.cancel();
    let (rest, _) = tokio::time::timeout(
        Duration::from_secs(10),
        until_event(&ev, |e| matches!(e, Event::RunDone { run_id: 1 })),
    )
    .await
    .expect("the drain outlived the cancel");
    seen.extend(rest);
    assert!(
        seen.iter().any(|e| matches!(
            e,
            Event::StatementDone {
                run_id: 1,
                stmt: 1,
                end: StatementEnd::Complete(_),
                ..
            }
        )),
        "{seen:?}"
    );
    assert!(
        seen.iter().any(|e| matches!(e, Event::SettleFailed { .. })),
        "{seen:?}"
    );
}
