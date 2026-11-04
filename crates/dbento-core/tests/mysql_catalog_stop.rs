//! MySQL catalog queries stuck behind DDL: the statement timeout and a cancel stop them, and
//! a catalog call dropped mid-wait kills its statement, so the session goes on either way.
mod support;
use std::time::{Duration, Instant};

use dbento_core::{
    DefinitionError, ExecuteError, IntrospectError, Limits, ObjectRef, RelationRef, SessionOptions,
};
use dbento_testkit::MyFlavor;
use mysql_async::prelude::*;
use support::{MyFixture, my_fixtures, my_thread_id, run, text};

/// Polls the processlist until a thread matches `filter`.
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

/// Starts `CREATE TABLE <name> AS SELECT SLEEP(60)`, which holds the new table's exclusive
/// metadata lock, so `SHOW CREATE TABLE` on it waits on both flavors (and MariaDB's
/// information_schema.TABLES too). Returns the blocker's thread id and its task.
async fn block_new_table(fx: &MyFixture, name: &str) -> (String, tokio::task::JoinHandle<()>) {
    let mut blocker = fx.db.admin().await;
    let id: u64 = blocker
        .query_first("SELECT CONNECTION_ID()")
        .await
        .unwrap()
        .unwrap();
    let sql = format!("CREATE TABLE {name} AS SELECT SLEEP(60) AS s");
    let task = tokio::spawn(async move {
        let _ = blocker.query_drop(sql).await;
    });
    wait_for(fx, &format!("ID = {id} AND STATE = 'User sleep'")).await;
    (id.to_string(), task)
}

/// Ends the blocker early, so the fixture's DROP DATABASE never waits on it.
async fn unblock(fx: &MyFixture, id: &str, task: tokio::task::JoinHandle<()>) {
    fx.db
        .admin()
        .await
        .query_drop(format!("KILL QUERY {id}"))
        .await
        .unwrap();
    task.await.unwrap();
}

fn stopped_with(r: &Result<dbento_core::Definition, DefinitionError>, timed: bool) -> bool {
    matches!(
        (r, timed),
        (Err(DefinitionError::Execute(ExecuteError::Timeout)), true)
            | (
                Err(DefinitionError::Execute(ExecuteError::Cancelled)),
                false
            )
    )
}

#[tokio::test]
async fn a_catalog_query_behind_ddl_stops_at_the_timeout_or_a_cancel() {
    for (label, fx) in my_fixtures("").await {
        let db = fx.db.database().to_owned();
        for timed in [true, false] {
            let mut opts = SessionOptions::default();
            if timed {
                opts.statement_timeout = Some(Duration::from_millis(1500));
            }
            let mut s = fx.session(opts).await;
            let me = my_thread_id(&mut s).await;
            let name = if timed { "t1" } else { "t2" };
            let (blocker, task) = block_new_table(&fx, name).await;
            let cancel = s.cancel_handle();
            let stop = async {
                if !timed {
                    wait_for(
                        &fx,
                        &format!("ID = {me} AND STATE LIKE 'Waiting for table metadata lock'"),
                    )
                    .await;
                    cancel.cancel();
                }
            };
            let started = Instant::now();
            let object = ObjectRef::relation(RelationRef::new(&db, name));
            let work = futures_util::future::join(s.definition(&object), stop);
            let (r, ()) = tokio::time::timeout(Duration::from_secs(20), work)
                .await
                .unwrap_or_else(|_| panic!("{label}: the catalog query was never stopped"));
            assert!(stopped_with(&r, timed), "{label}: timed {timed}: {r:?}");
            if timed {
                assert!(
                    started.elapsed() < Duration::from_secs(3),
                    "{label}: {:?}",
                    started.elapsed()
                );
            }
            // MariaDB's information_schema.TABLES waits behind the same lock.
            if timed && fx.db.flavor() == MyFlavor::Mariadb {
                let listed = tokio::time::timeout(Duration::from_secs(20), s.relations(&db))
                    .await
                    .unwrap_or_else(|_| panic!("{label}: the listing was never stopped"));
                assert!(
                    matches!(listed, Err(IntrospectError::Execute(ExecuteError::Timeout))),
                    "{label}: {listed:?}"
                );
            }
            assert!(s.schemas().await.is_ok(), "{label}");
            assert_eq!(
                run(&mut s, "SELECT 7", Limits::default())
                    .await
                    .remove(0)
                    .unwrap()
                    .rows,
                vec![vec![text("7")]],
                "{label}"
            );
            unblock(&fx, &blocker, task).await;
            s.close().await.unwrap();
        }
    }
}

#[tokio::test]
async fn a_dropped_catalog_call_kills_its_query_and_the_session_goes_on() {
    for (label, fx) in my_fixtures("").await {
        let db = fx.db.database().to_owned();
        let mut s = fx.session(SessionOptions::default()).await;
        let me = my_thread_id(&mut s).await;
        let (blocker, task) = block_new_table(&fx, "t3").await;
        let object = ObjectRef::relation(RelationRef::new(&db, "t3"));
        {
            let mut work = std::pin::pin!(s.definition(&object));
            let early = tokio::time::timeout(Duration::from_millis(800), &mut work).await;
            assert!(early.is_err(), "{label}: the catalog query did not wait");
        }
        assert!(
            fx.quiet_within(&me, Duration::from_secs(5)).await,
            "{label}: the catalog query runs on"
        );
        let next = run(&mut s, "SELECT 7", Limits::default()).await.remove(0);
        assert!(
            matches!(&next, Ok(r) if r.rows == vec![vec![text("7")]]),
            "{label}: {:?}",
            next.map(|r| r.rows)
        );
        unblock(&fx, &blocker, task).await;
        s.close().await.unwrap();
    }
}
