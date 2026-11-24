//! MySQL plain plans run under JSON v1 whatever the session's explain_json_format_version, and
//! the session reads its own value back afterwards: on error, in transactions, on cancel or drop.
use std::time::Duration;

use crate::my_explain::{JOIN, SETUP, a_of, req};
use crate::support::{Cell, MyFixture, assert_covers, my_fixtures, run};
use dbento_core::{ExecuteError, ExplainMode, Limits, Session, SessionOptions, Workspace, connect};
use dbento_testkit::MyFlavor;
use mysql_async::prelude::Queryable;

/// Sets the session to JSON v2 inside a procedure, so `settings_changed` stays off.
const V2_PROC: &str = "CREATE PROCEDURE v2() SET SESSION explain_json_format_version = 2";

async fn json_version(s: &mut Session) -> String {
    let sql = "SELECT CAST(@@session.explain_json_format_version AS CHAR)";
    match run(s, sql, Limits::default())
        .await
        .remove(0)
        .unwrap()
        .rows
        .remove(0)
        .remove(0)
    {
        Cell::Text(t) => t,
        other @ (Cell::Null | Cell::Bytes(_)) => panic!("{other:?}"),
    }
}

async fn ok(s: &mut Session, sql: &str) {
    run(s, sql, Limits::default()).await.remove(0).unwrap();
}

/// A MySQL session switched to JSON v2, as 9.x sessions start.
async fn on_v2(fx: &MyFixture, mut s: Session) -> Session {
    fx.exec(V2_PROC).await;
    ok(&mut s, "CALL v2()").await;
    assert_eq!(
        json_version(&mut s).await,
        "2",
        "CALL v2() did not switch the session"
    );
    s
}

#[tokio::test(flavor = "multi_thread")]
async fn a_v2_session_gets_v1_plans_and_keeps_its_version() {
    for (label, fx) in my_fixtures("").await {
        if fx.db.flavor() != MyFlavor::Mysql {
            continue;
        }
        fx.exec(SETUP).await;
        let mut s = on_v2(&fx, fx.session(SessionOptions::default()).await).await;
        let out = s
            .explain(&req(
                "UPDATE t SET a = a + 1 WHERE id = 1",
                ExplainMode::Plan,
            ))
            .await
            .unwrap();
        let p = out
            .plan
            .as_ref()
            .unwrap_or_else(|e| panic!("{label}: {e}: {}", out.raw));
        assert!(
            p.nodes.iter().any(|n| n.node_type == "Update"),
            "{label}: {}",
            out.raw
        );
        assert_eq!(json_version(&mut s).await, "2", "{label}: after a plan");

        let out = s.explain(&req(JOIN, ExplainMode::Analyze)).await.unwrap();
        assert!(
            out.plan.as_ref().is_ok_and(|p| p.analyzed),
            "{label}: {}",
            out.raw
        );

        let got = s
            .explain(&req("SELECT * FROM missing", ExplainMode::Plan))
            .await;
        assert!(
            matches!(got, Err(ExecuteError::Database { .. })),
            "{label}: {got:?}"
        );
        assert_eq!(json_version(&mut s).await, "2", "{label}: after an error");

        ok(&mut s, "START TRANSACTION READ ONLY").await;
        let out = s.explain(&req(JOIN, ExplainMode::Plan)).await.unwrap();
        assert!(out.plan.is_ok(), "{label}: {}", out.raw);
        assert_eq!(
            json_version(&mut s).await,
            "2",
            "{label}: in a READ ONLY transaction"
        );
        ok(&mut s, "ROLLBACK").await;

        // The SETs around the plan must not commit the user's open transaction.
        ok(&mut s, "START TRANSACTION").await;
        ok(&mut s, "INSERT INTO t VALUES (9, 90)").await;
        s.explain(&req(JOIN, ExplainMode::Plan)).await.unwrap();
        ok(&mut s, "ROLLBACK").await;
        assert_eq!(
            a_of(&fx, 9).await,
            None,
            "{label}: the plan committed the transaction"
        );

        assert!(
            !s.settings_changed(),
            "{label}: Dbento's own SET counted as the user's"
        );
        s.close().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_cancelled_or_dropped_plan_still_restores_the_version() {
    for (label, fx) in my_fixtures("").await {
        if fx.db.flavor() != MyFlavor::Mysql {
            continue;
        }
        fx.exec(SETUP).await;
        let mut s = on_v2(&fx, fx.session(SessionOptions::default()).await).await;
        // A write lock held elsewhere makes the EXPLAIN wait on its metadata lock.
        let mut lock = fx.db.admin().await;
        lock.query_drop("LOCK TABLES t WRITE").await.unwrap();
        let plan = req("SELECT * FROM t", ExplainMode::Plan);

        let cancel = s.cancel_handle();
        let t = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(300));
            cancel.cancel();
        });
        let got = tokio::time::timeout(Duration::from_secs(20), s.explain(&plan))
            .await
            .expect("the cancel was ignored");
        t.join().unwrap();
        assert!(
            matches!(got, Err(ExecuteError::Cancelled)),
            "{label}: {got:?}"
        );
        assert_eq!(json_version(&mut s).await, "2", "{label}: after a cancel");

        let dropped = tokio::time::timeout(Duration::from_millis(300), s.explain(&plan)).await;
        assert!(
            dropped.is_err(),
            "{label}: the EXPLAIN did not wait on the lock"
        );
        lock.query_drop("UNLOCK TABLES").await.unwrap();
        lock.disconnect().await.unwrap();
        assert_eq!(json_version(&mut s).await, "2", "{label}: after a drop");
        s.close().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_user_without_admin_privileges_gets_the_v1_pin_too() {
    for (label, fx) in my_fixtures("").await {
        if fx.db.flavor() != MyFlavor::Mysql {
            continue;
        }
        fx.exec(SETUP).await;
        let (db, user) = (
            fx.db.database().to_owned(),
            format!("x{}", fx.db.database()),
        );
        let who = format!("'{user}'@'%'");
        fx.exec(&format!(
            "DROP USER IF EXISTS {who}; CREATE USER {who} IDENTIFIED BY '{}'; GRANT SELECT, EXECUTE ON `{db}`.* TO {who}",
            fx.db.password()
        ))
        .await;
        let toml = fx.db.connection_toml("plain", "").replace(
            &format!("user = \"{}\"", fx.db.user()),
            &format!("user = \"{user}\""),
        );
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("dbento.toml"),
            format!("dbento = 1\n{toml}"),
        )
        .unwrap();
        let ws = Workspace::open(dir.path()).unwrap();
        let s = connect(
            &ws,
            ws.declared("plain").unwrap(),
            Some(fx.secret()),
            SessionOptions::default(),
        )
        .await
        .unwrap();
        let mut s = on_v2(&fx, s).await;
        let out = s.explain(&req(JOIN, ExplainMode::Plan)).await.unwrap();
        let p = out
            .plan
            .as_ref()
            .unwrap_or_else(|e| panic!("{label}: {e}: {}", out.raw));
        assert_covers(&label, &out.raw, p);
        assert_eq!(json_version(&mut s).await, "2", "{label}");
        s.close().await.unwrap();
        fx.exec(&format!("DROP USER {who}")).await;
    }
}
