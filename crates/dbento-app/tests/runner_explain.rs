//! The runner's Explain: asks only where ANALYZE could write, and is read in every wait.
use crate::support::{MANY, fixture, here, next_event, pg_spec, run_cmd, spec, until_event};
use dbento_app::runner::{Command, Event, StatementEnd, spawn_on};
use dbento_core::{Engine, ExecuteError, ExplainMode, Unguarded, explain_request};

fn explain(run_id: u64, engine: Engine, sql: &str, mode: ExplainMode) -> Command {
    Command::Explain {
        run_id,
        request: explain_request(engine, sql, mode).unwrap(),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_sqlite_explain_plans_without_asking_or_running() {
    let fx = fixture("CREATE TABLE t(x); INSERT INTO t VALUES (1);", "");
    let r = spawn_on(&here(), spec(&fx, "lite"));
    let ev = r.events();
    r.send(explain(
        1,
        Engine::Sqlite,
        "DELETE FROM t WHERE x > 0",
        ExplainMode::Analyze,
    ));
    assert!(matches!(next_event(&ev).await, Event::Connected { .. }));
    assert!(matches!(next_event(&ev).await,
        Event::Executing { run_id: 1, stmt: 0, sql } if sql == "EXPLAIN QUERY PLAN DELETE FROM t WHERE x > 0"));
    match next_event(&ev).await {
        Event::Planned {
            run_id: 1,
            result: Ok(out),
            ..
        } => assert!(out.raw.contains("SCAN t"), "{}", out.raw),
        other @ (Event::Connected { .. }
        | Event::ConnectFailed { .. }
        | Event::Executing { .. }
        | Event::StatementStarted { .. }
        | Event::NeedsConfirm { .. }
        | Event::Planned { .. }
        | Event::Batch { .. }
        | Event::Paused { .. }
        | Event::ParkedAtCap { .. }
        | Event::ParkExpired { .. }
        | Event::ParkReleased { .. }
        | Event::StatementDone { .. }
        | Event::RunDone { .. }
        | Event::Stopped { .. }
        | Event::SettingsChanged { .. }
        | Event::SettingsLost { .. }
        | Event::Disconnected { .. }
        | Event::Target { .. }
        | Event::Settling { .. }
        | Event::SettleFailed { .. }) => panic!("{other:?}"),
    }
    assert!(matches!(
        next_event(&ev).await,
        Event::RunDone { run_id: 1 }
    ));
    assert_eq!(fx.count("SELECT count(*) FROM t"), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_writing_analyze_on_a_writable_postgres_session_asks_first() {
    let (db, _dir, spec) = pg_spec("").await;
    db.admin()
        .await
        .batch_execute("CREATE TABLE t(a int); INSERT INTO t VALUES (1)")
        .await
        .unwrap();
    let r = spawn_on(&here(), spec);
    let ev = r.events();
    r.send(explain(
        1,
        Engine::Postgres,
        "  DELETE FROM t\n",
        ExplainMode::Analyze,
    ));
    assert!(matches!(
        next_event(&ev).await,
        Event::Connected {
            read_only: false,
            ..
        }
    ));
    match next_event(&ev).await {
        Event::NeedsConfirm {
            run_id: 1,
            stmt: 0,
            kinds,
            analyze: true,
            sql,
        } => {
            assert_eq!(kinds, [Unguarded::DeleteWithoutWhere]);
            assert_eq!(sql, "DELETE FROM t");
        }
        other @ (Event::Connected { .. }
        | Event::ConnectFailed { .. }
        | Event::Executing { .. }
        | Event::StatementStarted { .. }
        | Event::NeedsConfirm { .. }
        | Event::Planned { .. }
        | Event::Batch { .. }
        | Event::Paused { .. }
        | Event::ParkedAtCap { .. }
        | Event::ParkExpired { .. }
        | Event::ParkReleased { .. }
        | Event::StatementDone { .. }
        | Event::RunDone { .. }
        | Event::Stopped { .. }
        | Event::SettingsChanged { .. }
        | Event::SettingsLost { .. }
        | Event::Disconnected { .. }
        | Event::Target { .. }
        | Event::Settling { .. }
        | Event::SettleFailed { .. }) => panic!("{other:?}"),
    }
    r.send(Command::Skip { run_id: 1, stmt: 0 });
    assert!(matches!(
        next_event(&ev).await,
        Event::StatementDone {
            run_id: 1,
            end: StatementEnd::Skipped,
            ..
        }
    ));
    assert!(matches!(
        next_event(&ev).await,
        Event::RunDone { run_id: 1 }
    ));
    r.send(explain(
        2,
        Engine::Postgres,
        "DELETE FROM t",
        ExplainMode::Analyze,
    ));
    assert!(matches!(
        next_event(&ev).await,
        Event::NeedsConfirm {
            run_id: 2,
            analyze: true,
            ..
        }
    ));
    r.send(Command::Confirm { run_id: 2, stmt: 0 });
    assert!(matches!(
        next_event(&ev).await,
        Event::Executing { run_id: 2, .. }
    ));
    match next_event(&ev).await {
        Event::Planned {
            run_id: 2,
            result: Ok(out),
            ..
        } => assert!(out.plan.unwrap().analyzed),
        other @ (Event::Connected { .. }
        | Event::ConnectFailed { .. }
        | Event::Executing { .. }
        | Event::StatementStarted { .. }
        | Event::NeedsConfirm { .. }
        | Event::Planned { .. }
        | Event::Batch { .. }
        | Event::Paused { .. }
        | Event::ParkedAtCap { .. }
        | Event::ParkExpired { .. }
        | Event::ParkReleased { .. }
        | Event::StatementDone { .. }
        | Event::RunDone { .. }
        | Event::Stopped { .. }
        | Event::SettingsChanged { .. }
        | Event::SettingsLost { .. }
        | Event::Disconnected { .. }
        | Event::Target { .. }
        | Event::Settling { .. }
        | Event::SettleFailed { .. }) => panic!("{other:?}"),
    }
    assert!(matches!(
        next_event(&ev).await,
        Event::RunDone { run_id: 2 }
    ));
    assert_eq!(
        db.scalar("SELECT count(*)::text FROM t").await.as_deref(),
        Some("1")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn plain_explains_reads_and_read_only_sessions_never_ask() {
    let (db, _dir, spec) = pg_spec("read_only = true").await;
    db.admin()
        .await
        .batch_execute("CREATE TABLE t(a int); INSERT INTO t VALUES (1)")
        .await
        .unwrap();
    let r = spawn_on(&here(), spec);
    let ev = r.events();
    r.send(explain(
        1,
        Engine::Postgres,
        "DELETE FROM t",
        ExplainMode::Analyze,
    ));
    let (seen, _) = until_event(&ev, |e| matches!(e, Event::RunDone { run_id: 1 })).await;
    assert!(
        !seen.iter().any(|e| matches!(e, Event::NeedsConfirm { .. })),
        "{seen:?}"
    );
    assert!(
        seen.iter().any(|e| matches!(
            e,
            Event::Planned {
                result: Err(ExecuteError::ReadOnlyViolation { .. }),
                ..
            }
        )),
        "{seen:?}"
    );
    for (id, sql, mode) in [
        (2, "DELETE FROM t", ExplainMode::Plan),
        (3, "SELECT * FROM t", ExplainMode::Analyze),
    ] {
        r.send(explain(id, Engine::Postgres, sql, mode));
        let (seen, _) = until_event(
            &ev,
            |e| matches!(e, Event::RunDone { run_id } if *run_id == id),
        )
        .await;
        assert!(
            !seen.iter().any(|e| matches!(e, Event::NeedsConfirm { .. })),
            "{seen:?}"
        );
        assert!(
            seen.iter()
                .any(|e| matches!(e, Event::Planned { result: Ok(_), .. })),
            "{seen:?}"
        );
    }
    assert_eq!(
        db.scalar("SELECT count(*)::text FROM t").await.as_deref(),
        Some("1")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_explain_is_read_while_a_result_is_parked_or_a_question_is_open() {
    let fx = fixture("CREATE TABLE t(x); INSERT INTO t VALUES (1);", "");
    let r = spawn_on(&here(), spec(&fx, "lite"));
    let ev = r.events();
    r.send(run_cmd(1, MANY, Some(10)));
    until_event(&ev, |e| matches!(e, Event::ParkedAtCap { run_id: 1, .. })).await;
    r.send(explain(
        2,
        Engine::Sqlite,
        "SELECT * FROM t",
        ExplainMode::Plan,
    ));
    let (seen, _) = until_event(&ev, |e| matches!(e, Event::RunDone { run_id: 2 })).await;
    assert!(
        seen.iter().any(|e| matches!(
            e,
            Event::Planned {
                run_id: 2,
                result: Ok(_),
                ..
            }
        )),
        "{seen:?}"
    );
    r.send(run_cmd(3, "UPDATE t SET x = 0", None));
    until_event(&ev, |e| matches!(e, Event::NeedsConfirm { run_id: 3, .. })).await;
    r.send(explain(
        4,
        Engine::Sqlite,
        "SELECT * FROM t",
        ExplainMode::Plan,
    ));
    let (seen, _) = until_event(&ev, |e| matches!(e, Event::RunDone { run_id: 4 })).await;
    assert!(
        seen.iter().any(|e| matches!(
            e,
            Event::Planned {
                run_id: 4,
                result: Ok(_),
                ..
            }
        )),
        "{seen:?}"
    );
    assert_eq!(fx.count("SELECT count(*) FROM t WHERE x = 0"), 0);
}
