//! MariaDB `EXPLAIN FORMAT=JSON` and `ANALYZE FORMAT=JSON` read into plan nodes.
use crate::support::{assert_covers, my_explain_fixtures};
use dbento_core::ExplainOutput;

fn pairs(p: &[(&str, &str)]) -> Vec<(String, String)> {
    p.iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}

const UPDATE: &str = r#"{
  "query_optimization": {"r_total_time_ms": 0.049262322},
  "query_block": {"select_id": 1, "r_total_time_ms": 0.068088751,
    "table": {"update": 1, "table_name": "ord", "access_type": "range", "possible_keys": ["cust_id"],
      "key": "cust_id", "key_length": "5", "used_key_parts": ["cust_id"], "rows": 5, "r_rows": 5,
      "r_filtered": 100, "r_table_time_ms": 0.054006622, "r_other_time_ms": 0.009448395,
      "r_engine_stats": {"pages_accessed": 29, "pages_updated": 13},
      "attached_condition": "ord.cust_id = 5"}}}"#;

#[test]
fn an_analyzed_update_has_planning_execution_and_table_actuals() {
    let out = ExplainOutput::from_mariadb_json(UPDATE.to_owned());
    assert_eq!(out.raw, UPDATE);
    let plan = out.plan.unwrap();
    assert!(plan.analyzed);
    assert_eq!(
        (plan.planning_ms, plan.execution_ms),
        (Some(0.049262322), Some(0.068088751))
    );
    let [root, t] = &plan.nodes[..] else {
        panic!("two nodes")
    };
    assert_eq!((root.node_type.as_str(), root.depth), ("Query block", 0));
    assert_eq!(root.detail, pairs(&[("select_id", "1")]));
    let ra = root.actual.unwrap();
    assert_eq!((ra.total_ms, ra.rows, ra.loops), (0.068088751, 0.0, 1));
    assert_eq!(
        (
            t.node_type.as_str(),
            t.relation.as_deref(),
            t.index.as_deref(),
            t.rows
        ),
        ("Update", Some("ord"), Some("cust_id"), Some(5.0))
    );
    let ta = t.actual.unwrap();
    assert!(close(ta.total_ms, 0.054006622 + 0.009448395), "{ta:?}");
    assert_eq!((ta.rows, ta.loops), (5.0, 1));
    assert!(close(ra.self_ms, 0.068088751 - 0.054006622 - 0.009448395));
    assert_eq!(
        t.detail,
        pairs(&[
            ("update", "1"),
            ("access_type", "range"),
            ("possible_keys", "cust_id"),
            ("key_length", "5"),
            ("used_key_parts", "cust_id"),
            ("r_filtered", "100"),
            ("r_table_time_ms", "0.054006622"),
            ("r_other_time_ms", "0.009448395"),
            ("r_engine_stats.pages_accessed", "29"),
            ("r_engine_stats.pages_updated", "13"),
            ("attached_condition", "ord.cust_id = 5"),
        ])
    );
}

#[test]
fn loops_divide_total_times_and_zero_loops_never_ran() {
    let raw = r#"{"query_optimization": {"r_total_time_ms": 0.1},
      "query_block": {"select_id": 1, "cost": 0.14, "r_loops": 1, "r_total_time_ms": 3.0,
        "filesort": {"sort_key": "c.`name`", "r_loops": 1, "r_total_time_ms": 0.5, "r_output_rows": 20,
          "temporary_table": {"nested_loop": [
            {"table": {"table_name": "c", "access_type": "ref", "key": "region", "loops": 1, "r_loops": 1,
              "rows": 20, "r_rows": 20, "cost": 0.02, "r_table_time_ms": 0.2, "r_other_time_ms": 0.05,
              "filtered": 100, "r_filtered": 100}},
            {"table": {"table_name": "o", "access_type": "ref", "key": "cust_id", "loops": 20, "r_loops": 20,
              "rows": 5, "r_rows": 5, "cost": 0.12, "r_table_time_ms": 2.0, "r_other_time_ms": 0.4,
              "filtered": 100, "r_filtered": 100}},
            {"table": {"table_name": "x", "access_type": "ALL", "r_loops": 0, "rows": 3, "r_rows": null}}]}}}}"#;
    let plan = ExplainOutput::from_mariadb_json(raw.to_owned())
        .plan
        .unwrap();
    let shape: Vec<_> = plan
        .nodes
        .iter()
        .map(|n| (n.depth, n.node_type.as_str(), n.relation.as_deref()))
        .collect();
    assert_eq!(
        shape,
        [
            (0, "Query block", None),
            (1, "Filesort", None),
            (2, "Temporary table", None),
            (3, "Nested loop", None),
            (4, "Index lookup", Some("c")),
            (4, "Index lookup", Some("o")),
            (4, "Full scan", Some("x")),
        ]
    );
    assert_eq!(
        (plan.planning_ms, plan.execution_ms),
        (Some(0.1), Some(3.0))
    );
    assert_eq!(plan.nodes[0].total_cost, Some(0.14));
    assert_eq!(
        plan.nodes[1].detail,
        pairs(&[("sort_key", "c.`name`"), ("r_output_rows", "20")])
    );
    let fs = plan.nodes[1].actual.unwrap();
    assert_eq!((fs.total_ms, fs.rows, fs.loops), (0.5, 0.0, 1));
    assert!(plan.nodes[2].actual.is_none() && plan.nodes[3].actual.is_none());
    let c = &plan.nodes[4];
    assert_eq!(
        (c.index.as_deref(), c.rows, c.total_cost),
        (Some("region"), Some(20.0), Some(0.02))
    );
    assert!(close(c.actual.unwrap().total_ms, 0.25));
    assert_eq!(
        c.detail,
        pairs(&[
            ("access_type", "ref"),
            ("loops", "1"),
            ("r_table_time_ms", "0.2"),
            ("r_other_time_ms", "0.05"),
            ("filtered", "100"),
            ("r_filtered", "100"),
        ])
    );
    let o = plan.nodes[5].actual.unwrap();
    assert!(close(o.total_ms, 0.12), "{o:?}");
    assert_eq!((o.rows, o.loops), (5.0, 20));
    let x = plan.nodes[6].actual.unwrap();
    assert_eq!((x.total_ms, x.rows, x.loops), (0.0, 0.0, 0));
    assert_eq!(plan.nodes[6].detail, pairs(&[("access_type", "ALL")]));
}

#[test]
fn a_plain_plan_has_no_actuals_and_bad_output_keeps_the_raw_text() {
    let raw = r#"{"query_block": {"select_id": 1, "table": {"table_name": "ord"}}}"#;
    let plan = ExplainOutput::from_mariadb_json(raw.to_owned())
        .plan
        .unwrap();
    let shape: Vec<_> = plan
        .nodes
        .iter()
        .map(|n| (n.node_type.as_str(), n.relation.as_deref()))
        .collect();
    assert_eq!(shape, [("Query block", None), ("Table", Some("ord"))]);
    assert!(!plan.analyzed);
    assert_eq!((plan.planning_ms, plan.execution_ms), (None, None));
    for bad in ["{", r#"{"query_optimization": {"r_total_time_ms": 1}}"#] {
        let out = ExplainOutput::from_mariadb_json(bad.to_owned());
        assert_eq!(out.raw, bad);
        assert!(out.plan.is_err(), "{bad}");
    }
}

#[test]
fn every_captured_mariadb_plan_parses_with_nothing_dropped() {
    let all = my_explain_fixtures(&["mariadb"], ".json");
    assert!(all.len() >= 80, "{} MariaDB fixtures", all.len());
    for (name, raw) in all {
        let plan = ExplainOutput::from_mariadb_json(raw.clone())
            .plan
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_covers(&name, &raw, &plan);
        if name.contains("-plan-") {
            assert!(!plan.analyzed, "{name}: a plain plan has no actuals");
        }
    }
}
