//! MySQL `EXPLAIN FORMAT=JSON` (version 1) read into plan nodes, nothing dropped.
use crate::support::{assert_covers, my_explain_fixtures};
use dbento_core::ExplainOutput;

fn pairs(p: &[(&str, &str)]) -> Vec<(String, String)> {
    p.iter()
        .map(|(k, v)| ((*k).to_owned(), (*v).to_owned()))
        .collect()
}

const SELECT: &str = r#"{"query_block": {"select_id": 1, "cost_info": {"query_cost": "37.75"},
  "grouping_operation": {"using_temporary_table": true, "using_filesort": false, "nested_loop": [
    {"table": {"table_name": "c", "access_type": "ref", "possible_keys": ["PRIMARY", "region"],
      "key": "region", "used_key_parts": ["region"], "key_length": "5", "ref": ["const"],
      "rows_examined_per_scan": 20, "rows_produced_per_join": 20, "filtered": "100.00",
      "cost_info": {"read_cost": "0.75", "eval_cost": "2.00", "prefix_cost": "2.75", "data_read_per_join": "1K"},
      "used_columns": ["id", "name", "region"]}},
    {"table": {"table_name": "o", "access_type": "ref", "key": "cust_id", "rows_examined_per_scan": 5,
      "cost_info": {"prefix_cost": "37.75"}}}]}}}"#;

#[test]
fn a_json_plan_nests_blocks_operations_and_tables() {
    let out = ExplainOutput::from_mysql_json(SELECT.to_owned());
    assert_eq!(out.raw, SELECT);
    let plan = out.plan.unwrap();
    assert!(!plan.analyzed);
    assert_eq!((plan.planning_ms, plan.execution_ms), (None, None));
    let shape: Vec<_> = plan
        .nodes
        .iter()
        .map(|n| {
            (
                n.depth,
                n.parent,
                n.node_type.as_str(),
                n.relation.as_deref(),
            )
        })
        .collect();
    assert_eq!(
        shape,
        [
            (0, None, "Query block", None),
            (1, Some(0), "Grouping operation", None),
            (2, Some(1), "Nested loop", None),
            (3, Some(2), "Index lookup", Some("c")),
            (3, Some(2), "Index lookup", Some("o")),
        ]
    );
    let [root, group, _, c, o] = &plan.nodes[..] else {
        panic!("five nodes")
    };
    assert_eq!(root.total_cost, Some(37.75));
    assert_eq!(root.detail, pairs(&[("select_id", "1")]));
    assert_eq!(
        group.detail,
        pairs(&[
            ("using_temporary_table", "true"),
            ("using_filesort", "false")
        ])
    );
    assert_eq!(
        (c.index.as_deref(), c.rows, c.total_cost),
        (Some("region"), Some(20.0), Some(2.75))
    );
    assert_eq!(
        c.detail,
        pairs(&[
            ("access_type", "ref"),
            ("possible_keys", "PRIMARY, region"),
            ("used_key_parts", "region"),
            ("key_length", "5"),
            ("ref", "const"),
            ("rows_produced_per_join", "20"),
            ("filtered", "100.00"),
            ("cost_info.read_cost", "0.75"),
            ("cost_info.eval_cost", "2.00"),
            ("cost_info.data_read_per_join", "1K"),
            ("used_columns", "id, name, region"),
        ])
    );
    assert_eq!(
        (o.index.as_deref(), o.rows, o.total_cost),
        (Some("cust_id"), Some(5.0), Some(37.75))
    );
    assert_eq!(o.detail, pairs(&[("access_type", "ref")]));
}

#[test]
fn array_elements_with_several_keys_are_numbered_nodes() {
    let raw = r#"{"query_block": {"union_result": {"using_temporary_table": true, "table_name": "<union1,2>",
      "access_type": "ALL", "query_specifications": [
        {"dependent": false, "cacheable": true, "query_block": {"select_id": 1, "table": {"table_name": "cust", "access_type": "ref"}}},
        {"dependent": false, "cacheable": true, "query_block": {"select_id": 2, "table": {"table_name": "ord", "access_type": "ALL"}}}]}}}"#;
    let plan = ExplainOutput::from_mysql_json(raw.to_owned()).plan.unwrap();
    let shape: Vec<_> = plan
        .nodes
        .iter()
        .map(|n| (n.depth, n.node_type.as_str(), n.relation.as_deref()))
        .collect();
    assert_eq!(
        shape,
        [
            (0, "Query block", None),
            (1, "Union result", Some("<union1,2>")),
            (2, "Query specifications", None),
            (3, "Query specifications #1", None),
            (4, "Query block", None),
            (5, "Index lookup", Some("cust")),
            (3, "Query specifications #2", None),
            (4, "Query block", None),
            (5, "Full scan", Some("ord")),
        ]
    );
    assert_eq!(
        plan.nodes[1].detail,
        pairs(&[("using_temporary_table", "true"), ("access_type", "ALL")])
    );
    assert_eq!(
        plan.nodes[3].detail,
        pairs(&[("dependent", "false"), ("cacheable", "true")])
    );
    assert_eq!(plan.nodes[7].detail, pairs(&[("select_id", "2")]));
    assert_covers("union", raw, &plan);
}

#[test]
fn single_table_writes_are_named_by_their_flag() {
    let update = r#"{"query_block": {"select_id": 1, "table": {"update": true, "table_name": "ord",
      "access_type": "range", "key": "cust_id", "rows_examined_per_scan": 5, "filtered": "100.00",
      "attached_condition": "(`ord`.`cust_id` = 5)"}}}"#;
    let plan = ExplainOutput::from_mysql_json(update.to_owned())
        .plan
        .unwrap();
    let t = &plan.nodes[1];
    assert_eq!(
        (
            t.node_type.as_str(),
            t.relation.as_deref(),
            t.index.as_deref(),
            t.rows
        ),
        ("Update", Some("ord"), Some("cust_id"), Some(5.0))
    );
    assert_eq!(
        t.detail,
        pairs(&[
            ("update", "true"),
            ("access_type", "range"),
            ("filtered", "100.00"),
            ("attached_condition", "(`ord`.`cust_id` = 5)"),
        ])
    );
    let insert = r#"{"query_block": {"select_id": 1, "table": {"insert": true, "table_name": "ord", "access_type": "ALL"}}}"#;
    let plan = ExplainOutput::from_mysql_json(insert.to_owned())
        .plan
        .unwrap();
    assert_eq!(
        (
            plan.nodes[1].node_type.as_str(),
            plan.nodes[1].relation.as_deref()
        ),
        ("Insert", Some("ord"))
    );
}

#[test]
fn unknown_keys_become_nodes_or_detail_and_never_fail() {
    let raw = r#"{"query_block": {"select_id": 1, "new_list": [1, 2],
      "brand_new_thing": {"x": 1, "table": {"table_name": "t", "access_type": "ALL"}}}}"#;
    let plan = ExplainOutput::from_mysql_json(raw.to_owned()).plan.unwrap();
    let shape: Vec<_> = plan
        .nodes
        .iter()
        .map(|n| (n.depth, n.node_type.as_str()))
        .collect();
    assert_eq!(
        shape,
        [(0, "Query block"), (1, "Brand new thing"), (2, "Full scan")]
    );
    assert_eq!(
        plan.nodes[0].detail,
        pairs(&[("select_id", "1"), ("new_list", "[1,2]")])
    );
    assert_eq!(plan.nodes[1].detail, pairs(&[("x", "1")]));
}

#[test]
fn version_2_and_malformed_output_keep_the_raw_text() {
    let v2 = r#"{"query": "/* select#1 */ select 1 AS `1`", "operation": "Rows fetched before execution"}"#;
    let out = ExplainOutput::from_mysql_json(v2.to_owned());
    assert_eq!(out.raw, v2);
    let e = out.plan.unwrap_err();
    assert!(
        e.message.contains("explain_json_format_version"),
        "{}",
        e.message
    );
    for bad in ["{", "[1]", r#"{"query_block": 1}"#] {
        let out = ExplainOutput::from_mysql_json(bad.to_owned());
        assert_eq!(out.raw, bad);
        assert!(out.plan.is_err(), "{bad}");
    }
}

#[test]
fn every_captured_mysql_json_plan_parses_with_nothing_dropped() {
    let all = my_explain_fixtures(&["mysql"], ".json");
    assert!(all.len() >= 30, "{} MySQL JSON fixtures", all.len());
    for (name, raw) in all {
        let plan = ExplainOutput::from_mysql_json(raw.clone())
            .plan
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert!(!plan.nodes.is_empty(), "{name}");
        assert_covers(&name, &raw, &plan);
    }
}
