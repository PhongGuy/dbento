//! `Session::explain` on SQLite: EXPLAIN QUERY PLAN rows, never an execution.
mod support;
use dbento_core::{Engine, ExecuteError, ExplainMode, Limits, SessionOptions, explain_request};
use support::{MANY, sqlite_fixture};

const SETUP: &str =
    "CREATE TABLE t(id INTEGER PRIMARY KEY, a INTEGER); INSERT INTO t VALUES (1, 10), (2, 20);";

#[tokio::test]
async fn sqlite_plans_a_delete_without_running_it() {
    let fx = sqlite_fixture(SETUP, "");
    let mut s = fx.session(SessionOptions::default()).await;
    let r = explain_request(
        Engine::Sqlite,
        "DELETE FROM t WHERE a > 0",
        ExplainMode::Analyze,
    )
    .unwrap();
    let out = s.explain(&r).await.unwrap();
    assert!(
        out.raw.lines().all(|l| l.split('\t').count() == 4),
        "{:?}",
        out.raw
    );
    assert!(out.raw.contains("SCAN t"), "{:?}", out.raw);
    assert!(
        out.plan
            .unwrap()
            .nodes
            .iter()
            .any(|n| n.node_type.starts_with("SCAN t"))
    );
    assert_eq!(fx.count("SELECT count(*) FROM t"), 2);
}

#[tokio::test]
async fn a_dropped_result_is_settled_first_and_engines_must_match() {
    let fx = sqlite_fixture(SETUP, "");
    let mut s = fx.session(SessionOptions::default()).await;
    {
        let mut script = s.script(MANY);
        let p = script.next().await.unwrap().unwrap();
        let mut stream = p.execute(Limits::default()).await.unwrap();
        assert!(stream.next_batch().await.unwrap().is_some());
    }
    let r = explain_request(
        Engine::Sqlite,
        "SELECT * FROM t WHERE a = 10",
        ExplainMode::Plan,
    )
    .unwrap();
    assert!(s.explain(&r).await.unwrap().raw.contains("t"));
    let pg = explain_request(Engine::Postgres, "SELECT 1", ExplainMode::Plan).unwrap();
    assert!(matches!(
        s.explain(&pg).await,
        Err(ExecuteError::Unsupported { .. })
    ));
}

#[tokio::test]
async fn a_request_whose_text_was_changed_after_its_check_is_refused() {
    let fx = sqlite_fixture(SETUP, "");
    let mut s = fx.session(SessionOptions::default()).await;
    let mut r = explain_request(Engine::Sqlite, "SELECT * FROM t", ExplainMode::Plan).unwrap();
    r.sql.push_str("; DELETE FROM t");
    assert!(matches!(
        s.explain(&r).await,
        Err(ExecuteError::Unsupported { .. })
    ));
    assert_eq!(fx.count("SELECT count(*) FROM t"), 2);
}

#[tokio::test]
async fn a_request_whose_sql_is_not_an_explain_is_refused_unrun() {
    let fx = sqlite_fixture(SETUP, "");
    let mut s = fx.session(SessionOptions::default()).await;
    let mut r = explain_request(Engine::Sqlite, "SELECT * FROM t", ExplainMode::Plan).unwrap();
    r.sql = "DELETE FROM t".to_owned();
    assert!(matches!(
        s.explain(&r).await,
        Err(ExecuteError::Unsupported { .. })
    ));
    assert_eq!(fx.count("SELECT count(*) FROM t"), 2);
}
