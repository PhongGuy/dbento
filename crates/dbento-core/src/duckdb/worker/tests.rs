use std::sync::Arc;
use std::time::Duration;

use ::duckdb::Connection;
use tokio::sync::{mpsc, oneshot};

use super::{Shared, StatementJob, execute};
use crate::duckdb::cancel::DuckCancel;
use crate::session::{ExecuteError, Limits};

/// Runs `sql` through `execute` with a cancel that reached it after `begin` and whose every
/// fire was cleared by DuckDB, as when prepare and execution start swallow them all.
fn execute_after_a_swallowed_cancel(sql: &str) -> Option<Result<usize, ExecuteError>> {
    let conn = Connection::open_in_memory().unwrap();
    let shared = Shared::new(false, true);
    let cancel = DuckCancel::start(Duration::from_secs(3600)).unwrap();
    let generation = cancel.next_generation();
    cancel.begin(generation, None, Arc::new(|| {})).unwrap();
    cancel.cancel();
    let (tx, mut rx) = oneshot::channel();
    let (events, _keep) = mpsc::channel(16);
    let job = StatementJob {
        text: sql,
        generation,
        timeout: None,
        limits: Limits::default(),
        resume: None,
    };
    let mut reply = Some(tx);
    let ran = execute(&conn, &shared, &cancel, job, &mut reply, &events);
    cancel.end();
    cancel.shut();
    match (ran, reply) {
        (Err(e), Some(_)) => Some(Err(e)),
        _ => rx.try_recv().ok().map(|r| r.map(|c| c.len())),
    }
}

#[test]
fn a_cancel_before_the_columns_fails_a_select_at_execute() {
    let answer = execute_after_a_swallowed_cancel("SELECT 1");
    assert!(
        matches!(answer, Some(Err(ExecuteError::Cancelled))),
        "{answer:?}"
    );
}

#[test]
fn a_cancel_before_the_columns_keeps_an_applied_write_reported() {
    let answer = execute_after_a_swallowed_cancel("CREATE TABLE t AS SELECT 1 AS a");
    assert!(matches!(answer, Some(Ok(_))), "{answer:?}");
}
