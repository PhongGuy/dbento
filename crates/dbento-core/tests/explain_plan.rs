//! Plans parsed from captured Postgres 17 output and from SQLite rows.
use dbento_core::{ExplainOutput, Plan};

const PLAIN: &str = include_str!("fixtures/explain/pg17_plain.json");
const ANALYZE: &str = include_str!("fixtures/explain/pg17_analyze_buffers.json");
const GATHER: &str = include_str!("fixtures/explain/pg17_gather.json");
const INITPLAN: &str = include_str!("fixtures/explain/pg17_initplan.json");

fn near(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-9
}
fn has(n: &dbento_core::PlanNode, key: &str, value: &str) -> bool {
    n.detail.iter().any(|(k, v)| k == key && v == value)
}

#[test]
fn a_plain_plan_has_estimates_and_no_actuals() {
    let p = Plan::from_postgres_json(PLAIN).unwrap();
    assert!(!p.analyzed);
    assert_eq!((p.planning_ms, p.execution_ms), (None, None));
    assert_eq!(p.nodes.len(), 1);
    let n = &p.nodes[0];
    assert_eq!(n.node_type, "Index Scan");
    assert_eq!(
        (
            n.relation.as_deref(),
            n.index.as_deref(),
            n.alias.as_deref()
        ),
        (Some("t"), Some("t_pkey"), Some("t"))
    );
    assert_eq!((n.startup_cost, n.total_cost), (Some(0.29), Some(8.30)));
    assert_eq!((n.rows, n.width), (Some(1.0), Some(41)));
    assert_eq!((n.parent, n.depth, n.actual), (None, 0, None));
    assert!(has(n, "Index Cond", "(id = 1)"));
    assert!(has(n, "Parallel Aware", "false"));
    assert!(
        !n.detail
            .iter()
            .any(|(k, _)| k == "Node Type" || k == "Total Cost" || k == "Plans")
    );
    assert_eq!(p.self_share(0), None);
}

#[test]
fn analyze_with_buffers_has_actuals_self_time_and_buffers() {
    let p = Plan::from_postgres_json(ANALYZE).unwrap();
    assert!(p.analyzed);
    assert_eq!((p.planning_ms, p.execution_ms), (Some(1.228), Some(3.062)));
    let shape: Vec<_> = p
        .nodes
        .iter()
        .map(|n| (n.node_type.as_str(), n.depth, n.parent))
        .collect();
    assert_eq!(
        shape,
        [
            ("Sort", 0, None),
            ("Hash Join", 1, Some(0)),
            ("Seq Scan", 2, Some(1)),
            ("Hash", 2, Some(1)),
            ("Seq Scan", 3, Some(3))
        ]
    );
    assert_eq!(p.nodes[1].join_type.as_deref(), Some("Inner"));
    assert!(has(&p.nodes[0], "Sort Key", "t.id"));
    let scan = &p.nodes[4];
    assert_eq!(scan.relation.as_deref(), Some("t"));
    let a = scan.actual.unwrap();
    assert!(
        near(a.total_ms, 1.458) && near(a.rows, 2000.0) && a.loops == 1 && near(a.self_ms, 1.458)
    );
    assert_eq!((scan.shared_hit, scan.shared_read), (Some(187), Some(0)));
    assert!(has(scan, "Filter", "(a < 10)"));
    assert!(near(
        p.nodes[1].actual.unwrap().self_ms,
        2.769 - 0.423 - 1.823
    ));
    assert!(near(p.self_share(4).unwrap(), 1.458 / 3.062));
}

#[test]
fn parallel_children_count_every_loop() {
    let p = Plan::from_postgres_json(GATHER).unwrap();
    let names: Vec<_> = p.nodes.iter().map(|n| n.node_type.as_str()).collect();
    assert_eq!(names, ["Aggregate", "Gather", "Aggregate", "Seq Scan"]);
    let scan = p.nodes[3].actual.unwrap();
    assert_eq!(scan.loops, 3);
    assert!(near(scan.self_ms, 0.62 * 3.0));
    assert!(near(
        p.nodes[1].actual.unwrap().self_ms,
        7.845 - 0.644 * 3.0
    ));
    assert!(has(&p.nodes[1], "Workers Launched", "2"));
}

#[test]
fn an_init_plan_is_a_child_of_the_node_that_uses_it() {
    let p = Plan::from_postgres_json(INITPLAN).unwrap();
    assert_eq!(p.nodes.len(), 6);
    assert_eq!(
        (p.nodes[1].node_type.as_str(), p.nodes[1].parent),
        ("Aggregate", Some(0))
    );
    assert!(has(&p.nodes[1], "Parent Relationship", "InitPlan"));
    assert!(has(&p.nodes[1], "Subplan Name", "InitPlan 1"));
    assert!(near(p.nodes[0].actual.unwrap().self_ms, 4.807 - 4.736));
}

#[test]
fn self_time_and_share_are_clamped() {
    let raw = r#"[{"Plan": {"Node Type": "Gather", "Actual Total Time": 1.0, "Actual Rows": 1, "Actual Loops": 1,
        "Plans": [{"Node Type": "Seq Scan", "Actual Total Time": 2.0, "Actual Rows": 1.5, "Actual Loops": 1}]},
        "Planning Time": 0.1, "Execution Time": 1.5}]"#;
    let p = Plan::from_postgres_json(raw).unwrap();
    assert_eq!(p.nodes[0].actual.unwrap().self_ms, 0.0);
    assert_eq!(p.self_share(0), Some(0.0));
    assert_eq!(p.self_share(1), Some(1.0));
    assert!(near(p.nodes[1].actual.unwrap().rows, 1.5));
}

#[test]
fn unknown_fields_are_kept_as_text_in_the_order_received() {
    let raw = r#"[{"Plan": {"Node Type": "Custom Scan", "Zeta": 1, "Alpha": ["x", "y"], "Nested": {"k": true}, "Nothing": null}}]"#;
    let p = Plan::from_postgres_json(raw).unwrap();
    let want: Vec<(String, String)> = [
        ("Zeta", "1"),
        ("Alpha", "x, y"),
        ("Nested", r#"{"k":true}"#),
        ("Nothing", "null"),
    ]
    .iter()
    .map(|(k, v)| (k.to_string(), v.to_string()))
    .collect();
    assert_eq!(p.nodes[0].detail, want);
}

#[test]
fn malformed_output_is_an_error_and_keeps_the_raw_text() {
    for raw in [
        "",
        "{",
        "[]",
        r#"[{"Planning Time": 1}]"#,
        r#"[{"Plan": {"Total Cost": 1}}]"#,
    ] {
        let e = Plan::from_postgres_json(raw).unwrap_err();
        assert!(!e.message.is_empty(), "{raw:?}");
        let out = ExplainOutput::from_postgres(raw.to_owned());
        assert_eq!(out.raw, raw);
        assert!(out.plan.is_err());
    }
}

#[test]
fn sqlite_rows_nest_by_parent_and_orphans_become_roots() {
    let rows = vec![
        (2, 0, "SCAN t".to_owned()),
        (5, 0, "CORRELATED SCALAR SUBQUERY 1".to_owned()),
        (9, 5, "SEARCH u USING INDEX i (a=?)".to_owned()),
        (12, 77, "USE TEMP B-TREE FOR ORDER BY".to_owned()),
    ];
    let p = Plan::from_sqlite_rows(&rows);
    let shape: Vec<_> = p
        .nodes
        .iter()
        .map(|n| (n.node_type.as_str(), n.depth, n.parent))
        .collect();
    assert_eq!(
        shape,
        [
            ("SCAN t", 0, None),
            ("CORRELATED SCALAR SUBQUERY 1", 0, None),
            ("SEARCH u USING INDEX i (a=?)", 1, Some(1)),
            ("USE TEMP B-TREE FOR ORDER BY", 0, None)
        ]
    );
    assert!(
        p.nodes
            .iter()
            .all(|n| n.total_cost.is_none() && n.actual.is_none() && n.detail.is_empty())
    );
    assert!(!p.analyzed);
    assert!(Plan::from_sqlite_rows(&[]).nodes.is_empty());
    let out = ExplainOutput::from_sqlite(&[
        (2, 0, 0, "SCAN t".to_owned()),
        (3, 0, 0, "SCAN u".to_owned()),
    ]);
    assert_eq!(out.raw, "2\t0\t0\tSCAN t\n3\t0\t0\tSCAN u");
    assert_eq!(out.plan.unwrap().nodes.len(), 2);
}
