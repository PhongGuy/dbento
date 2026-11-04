//! The explain request: one checked statement and the exact text each engine is sent.
use dbento_core::{
    Engine, ExplainError, ExplainMode, Unguarded, Writes, explain_request, explain_text,
};

#[test]
fn postgres_wraps_the_trimmed_statement_in_json_explain() {
    let plan =
        explain_request(Engine::Postgres, "  SELECT * FROM t;\n", ExplainMode::Plan).unwrap();
    assert_eq!(plan.sql, "EXPLAIN (FORMAT JSON) SELECT * FROM t;");
    assert_eq!(plan.statement, "  SELECT * FROM t;\n");
    assert_eq!(plan.mode, ExplainMode::Plan);
    assert!(!plan.analyze_unavailable);
    assert!(plan.classification.is_none());
    let analyze = explain_request(Engine::Postgres, "SELECT 1", ExplainMode::Analyze).unwrap();
    assert_eq!(
        analyze.sql,
        "EXPLAIN (ANALYZE, BUFFERS, FORMAT JSON) SELECT 1"
    );
    assert_eq!(analyze.mode, ExplainMode::Analyze);
}

#[test]
fn postgres_analyze_classifies_the_inner_statement() {
    let class = |sql| {
        explain_request(Engine::Postgres, sql, ExplainMode::Analyze)
            .unwrap()
            .classification
            .expect("Postgres ANALYZE carries a classification")
    };
    assert_eq!(class("SELECT 1").writes, Writes::No);
    let delete = class("DELETE FROM t");
    assert_eq!(delete.writes, Writes::Yes);
    assert_eq!(delete.unguarded, [Unguarded::DeleteWithoutWhere]);
    assert_eq!(
        class("WITH d AS (DELETE FROM t RETURNING *) SELECT * FROM d").writes,
        Writes::Yes
    );
    assert_eq!(class("SELECT ((").writes, Writes::Unknown);
}

#[test]
fn sqlite_uses_query_plan_for_both_modes() {
    let plan = explain_request(Engine::Sqlite, "SELECT 1", ExplainMode::Plan).unwrap();
    assert_eq!(plan.sql, "EXPLAIN QUERY PLAN SELECT 1");
    assert!(!plan.analyze_unavailable);
    let a = explain_request(Engine::Sqlite, "SELECT 1", ExplainMode::Analyze).unwrap();
    assert_eq!(a.sql, "EXPLAIN QUERY PLAN SELECT 1");
    assert_eq!(a.mode, ExplainMode::Plan);
    assert!(a.analyze_unavailable);
    assert!(a.classification.is_none());
}

#[test]
fn refusals_come_in_check_order() {
    for engine in [Engine::Postgres, Engine::Sqlite] {
        let e = |sql| explain_request(engine, sql, ExplainMode::Plan).unwrap_err();
        assert_eq!(e("SELECT 1; SELECT 2"), ExplainError::NotOneStatement);
        assert_eq!(e("   "), ExplainError::NotOneStatement);
        assert_eq!(e("EXPLAIN SELECT 1"), ExplainError::AlreadyExplain);
        assert_eq!(
            e("-- c\n/* x */ explain analyze delete from t"),
            ExplainError::AlreadyExplain
        );
        assert_eq!(e("CREATE TABLE x(a int)"), ExplainError::NotExplainable);
        assert_eq!(e("/* lead */ SELECT 1"), ExplainError::NotExplainable);
    }
    assert_eq!(
        ExplainError::AlreadyExplain.to_string(),
        "already an EXPLAIN; run it normally"
    );
    assert_eq!(
        ExplainError::NotOneStatement.to_string(),
        "select one statement to explain"
    );
}

#[test]
fn the_moved_allowlist_keeps_its_cases() {
    for ok in [
        "SELECT 1",
        "  -- a\n--b\n with x as (select 1) select * from x",
        "delete from t",
    ] {
        assert!(explain_text(Engine::Postgres, ok).is_ok(), "{ok:?}");
    }
    for no in [
        "/* */ SELECT 1",
        "ANALYZE DELETE",
        "(ANALYZE) DELETE",
        "SELECT_x",
        "-- only",
        "",
        "-- x\rANALYZE DELETE FROM t\nSELECT 1",
        "\u{a0}SELECT 1",
    ] {
        assert!(explain_text(Engine::Postgres, no).is_err(), "{no:?}");
    }
}

#[test]
fn explain_text_is_the_mcp_plain_form() {
    assert_eq!(
        explain_text(Engine::Postgres, " SELECT * FROM t ").unwrap(),
        "EXPLAIN SELECT * FROM t"
    );
    assert_eq!(
        explain_text(Engine::Sqlite, "-- by id\nSELECT 1").unwrap(),
        "EXPLAIN QUERY PLAN -- by id\nSELECT 1"
    );
}
