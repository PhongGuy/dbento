//! EXPLAIN on MySQL and MariaDB: JSON plans, ANALYZE only of confirmed reads, the exact forms
//! re-checked before anything is sent, and a read-only session refusing plans of writes.
use crate::my_explain_capture::{SHAPES, SHAPES_SETUP};
use crate::support::{MyFixture, assert_covers, my_fixtures};
use dbento_core::{
    Engine, ExecuteError, ExplainMode, ExplainRequest, SessionOptions, explain_request,
};
use dbento_testkit::MyFlavor;

pub(crate) const SETUP: &str =
    "CREATE TABLE t (id INT PRIMARY KEY, a INT, KEY a (a)) ENGINE=InnoDB;
CREATE TABLE u (id INT PRIMARY KEY, t_id INT) ENGINE=InnoDB;
INSERT INTO t VALUES (1, 10), (2, 20), (3, 30); INSERT INTO u VALUES (1, 1), (2, 3);";

pub(crate) fn req(sql: &str, mode: ExplainMode) -> ExplainRequest {
    explain_request(Engine::Mysql, sql, mode).unwrap()
}

pub(crate) async fn a_of(fx: &MyFixture, id: i32) -> Option<String> {
    fx.db
        .scalar(&format!("SELECT CAST(a AS CHAR) FROM t WHERE id = {id}"))
        .await
}

pub(crate) const JOIN: &str = "SELECT * FROM t JOIN u ON u.t_id = t.id WHERE t.a > 5";

#[tokio::test(flavor = "multi_thread")]
async fn reads_are_planned_and_analyzed_and_writes_only_planned() {
    for (label, fx) in my_fixtures("").await {
        fx.exec(SETUP).await;
        let maria = fx.db.flavor() == MyFlavor::Mariadb;
        let mut s = fx.session(SessionOptions::default()).await;

        let out = s.explain(&req(JOIN, ExplainMode::Plan)).await.unwrap();
        let p = out
            .plan
            .as_ref()
            .unwrap_or_else(|e| panic!("{label}: {e}: {}", out.raw));
        assert!(!p.analyzed, "{label}");
        assert!(
            p.nodes.iter().any(|n| n.relation.as_deref() == Some("t")),
            "{label}: {}",
            out.raw
        );
        assert_covers(&label, &out.raw, p);

        let read = req(JOIN, ExplainMode::Analyze);
        let form = if maria {
            "ANALYZE FORMAT=JSON"
        } else {
            "EXPLAIN ANALYZE FORMAT=TREE"
        };
        assert_eq!(
            s.explain_sql(&read).unwrap(),
            format!("{form} {JOIN}"),
            "{label}"
        );
        let out = s.explain(&read).await.unwrap();
        let p = out
            .plan
            .as_ref()
            .unwrap_or_else(|e| panic!("{label}: {e}: {}", out.raw));
        assert!(
            p.analyzed && p.nodes[0].actual.is_some(),
            "{label}: {}",
            out.raw
        );

        let write = req("UPDATE t SET a = a + 1 WHERE id = 1", ExplainMode::Analyze);
        assert_eq!(
            (write.mode, write.analyze_unavailable),
            (ExplainMode::Plan, true),
            "{label}"
        );
        assert_eq!(
            s.explain_sql(&write).unwrap(),
            "EXPLAIN FORMAT=JSON UPDATE t SET a = a + 1 WHERE id = 1"
        );
        let out = s.explain(&write).await.unwrap();
        let p = out
            .plan
            .as_ref()
            .unwrap_or_else(|e| panic!("{label}: {e}: {}", out.raw));
        assert!(
            !p.analyzed && p.nodes.iter().any(|n| n.node_type == "Update"),
            "{label}: {}",
            out.raw
        );
        s.close().await.unwrap();
        assert_eq!(a_of(&fx, 1).await.as_deref(), Some("10"), "{label}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_statement_the_classifier_cannot_read_is_only_planned() {
    for (label, fx) in my_fixtures("").await {
        if fx.db.flavor() != MyFlavor::Mysql {
            continue;
        }
        fx.exec(SETUP).await;
        let mut s = fx.session(SessionOptions::default()).await;
        let r = req("TABLE t", ExplainMode::Analyze);
        let out = s.explain(&r).await.unwrap();
        assert!(
            out.plan.as_ref().is_ok_and(|p| !p.analyzed),
            "{label}: {}",
            out.raw
        );
        s.close().await.unwrap();
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_request_edited_after_its_check_is_refused_unsent() {
    for (label, fx) in my_fixtures("").await {
        fx.exec(SETUP).await;
        let mut s = fx.session(SessionOptions::default()).await;
        let mut cases = Vec::new();
        let mut r = req("SELECT * FROM t", ExplainMode::Plan);
        r.sql = "ANALYZE TABLE t".to_owned();
        cases.push(r);
        let mut r = req("SELECT * FROM t", ExplainMode::Plan);
        r.sql = "EXPLAIN FORMAT=JSON SELECT * FROM t WHERE id = 2".to_owned();
        cases.push(r);
        let mut r = req("SELECT * FROM t", ExplainMode::Analyze);
        r.statement = "UPDATE t SET a = 0 WHERE id = 1".to_owned();
        r.sql = "EXPLAIN ANALYZE FORMAT=TREE UPDATE t SET a = 0 WHERE id = 1".to_owned();
        cases.push(r);
        let mut r = req("SELECT * FROM t", ExplainMode::Analyze);
        r.statement = "TABLE t".to_owned();
        r.sql = "EXPLAIN ANALYZE FORMAT=TREE TABLE t".to_owned();
        cases.push(r);
        let mut r = req("SELECT * FROM t", ExplainMode::Analyze);
        r.sql = "EXPLAIN ANALYZE FORMAT=JSON SELECT * FROM t".to_owned();
        cases.push(r);
        cases.push(explain_request(Engine::Postgres, "SELECT 1", ExplainMode::Plan).unwrap());
        for r in &cases {
            assert!(
                matches!(s.explain_sql(r), Err(ExecuteError::Unsupported { .. })),
                "{label}: {}",
                r.sql
            );
            assert!(
                matches!(s.explain(r).await, Err(ExecuteError::Unsupported { .. })),
                "{label}: {}",
                r.sql
            );
        }
        s.close().await.unwrap();
        assert_eq!(a_of(&fx, 1).await.as_deref(), Some("10"), "{label}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_read_only_session_refuses_plans_of_writes_and_analyzes_reads() {
    for (label, fx) in my_fixtures("read_only = true").await {
        fx.exec(SETUP).await;
        let mut s = fx.session(SessionOptions::default()).await;
        for sql in [
            "UPDATE t SET a = 0 WHERE id = 1",
            "DELETE FROM t WHERE id = 1",
            "INSERT INTO t VALUES (9, 90)",
            "REPLACE INTO t VALUES (1, 0)",
        ] {
            let got = s.explain(&req(sql, ExplainMode::Plan)).await;
            assert!(
                matches!(got, Err(ExecuteError::ReadOnlyViolation { .. })),
                "{label}: {sql}: {got:?}"
            );
        }
        let out = s
            .explain(&req("SELECT COUNT(*) FROM t", ExplainMode::Analyze))
            .await
            .unwrap();
        assert!(
            out.plan.as_ref().is_ok_and(|p| p.analyzed),
            "{label}: {}",
            out.raw
        );
        s.close().await.unwrap();
        assert_eq!(a_of(&fx, 1).await.as_deref(), Some("10"), "{label}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn every_shape_parses_live_with_nothing_dropped() {
    for (label, fx) in my_fixtures("").await {
        fx.exec(SHAPES_SETUP).await;
        let maria = fx.db.flavor() == MyFlavor::Mariadb;
        let mut s = fx.session(SessionOptions::default()).await;
        for (name, sql, reads) in SHAPES {
            let modes: &[ExplainMode] = if *reads {
                &[ExplainMode::Plan, ExplainMode::Analyze]
            } else {
                &[ExplainMode::Plan]
            };
            for &mode in modes {
                let out = s
                    .explain(&req(sql, mode))
                    .await
                    .unwrap_or_else(|e| panic!("{label} {name} {mode:?}: {e}"));
                let plan = out
                    .plan
                    .as_ref()
                    .unwrap_or_else(|e| panic!("{label} {name} {mode:?}: {e}\n{}", out.raw));
                if maria || mode == ExplainMode::Plan {
                    assert_covers(&format!("{label} {name} {mode:?}"), &out.raw, plan);
                }
            }
        }
        s.close().await.unwrap();
    }
}
