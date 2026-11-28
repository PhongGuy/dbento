//! MySQL `EXPLAIN ANALYZE` tree text read into plan nodes, one per line.
use crate::support::my_explain_fixtures;
use dbento_core::ExplainOutput;

const ANALYZE: &str = "-> Table scan on <temporary>  (actual time=0.71..0.712 rows=20 loops=1)
    -> Aggregate using temporary table  (actual time=0.709..0.709 rows=20 loops=1)
        -> Nested loop inner join  (cost=37.8 rows=100) (actual time=0.285..0.42 rows=100 loops=1)
            -> Index lookup on c using region (region=3)  (cost=2.75 rows=20) (actual time=0.267..0.27 rows=20 loops=1)
            -> Index lookup on o using cust_id (cust_id=c.id)  (cost=1.27 rows=5) (actual time=0.00631..0.00702 rows=5 loops=20)

";

fn close(a: f64, b: f64) -> bool {
    (a - b).abs() < 1e-6
}

#[test]
fn each_line_is_a_node_with_its_estimate_and_per_loop_actuals() {
    let out = ExplainOutput::from_mysql_tree(ANALYZE.to_owned());
    assert_eq!(out.raw, ANALYZE);
    let plan = out.plan.unwrap();
    assert!(plan.analyzed);
    let shape: Vec<_> = plan
        .nodes
        .iter()
        .map(|n| (n.depth, n.parent, n.node_type.as_str()))
        .collect();
    assert_eq!(
        shape,
        [
            (0, None, "Table scan on <temporary>"),
            (1, Some(0), "Aggregate using temporary table"),
            (2, Some(1), "Nested loop inner join"),
            (3, Some(2), "Index lookup on c using region (region=3)"),
            (3, Some(2), "Index lookup on o using cust_id (cust_id=c.id)"),
        ]
    );
    assert_eq!((plan.nodes[0].total_cost, plan.nodes[0].rows), (None, None));
    assert_eq!(
        (plan.nodes[2].total_cost, plan.nodes[2].rows),
        (Some(37.8), Some(100.0))
    );
    let o = plan.nodes[4].actual.unwrap();
    assert_eq!((o.total_ms, o.rows, o.loops), (0.00702, 5.0, 20));
    let root = plan.nodes[0].actual.unwrap();
    assert_eq!((root.total_ms, root.rows, root.loops), (0.712, 20.0, 1));
    assert_eq!(plan.execution_ms, Some(0.712));
    assert_eq!(plan.planning_ms, None);
    // The join's own time: 0.42 less 0.27 and 20 loops of 0.00702.
    assert!(close(
        plan.nodes[2].actual.unwrap().self_ms,
        0.42 - 0.27 - 0.1404
    ));
}

#[test]
fn a_plain_tree_never_executed_nodes_and_odd_labels_parse() {
    let plain = "-> Nested loop inner join  (cost=37.8 rows=100)\n    -> Table scan on c  (cost=0.25..2.75 rows=20)\n";
    let plan = ExplainOutput::from_mysql_tree(plain.to_owned())
        .plan
        .unwrap();
    assert!(!plan.analyzed && plan.execution_ms.is_none());
    assert_eq!(
        (plan.nodes[1].startup_cost, plan.nodes[1].total_cost),
        (Some(0.25), Some(2.75))
    );

    let never =
        "-> Nested loop inner join  (cost=1.1 rows=1) (actual time=0.05..0.05 rows=0 loops=1)
    -> Table scan on a  (cost=0.35 rows=1) (actual time=0.03..0.03 rows=0 loops=1)
    -> Index lookup on b using k (k=a.id)  (cost=0.35 rows=1) (never executed)";
    let plan = ExplainOutput::from_mysql_tree(never.to_owned())
        .plan
        .unwrap();
    let b = plan.nodes[2].actual.unwrap();
    assert_eq!((b.total_ms, b.rows, b.loops), (0.0, 0.0, 0));
    assert_eq!(
        plan.nodes[2].node_type,
        "Index lookup on b using k (k=a.id)"
    );

    let odd = "-> Filter: (t.s = '(cost=1')  (actual time=0.1..0.2 rows=1 loops=1)\n    -> Filter: (t.a > 5)\n";
    let plan = ExplainOutput::from_mysql_tree(odd.to_owned()).plan.unwrap();
    assert_eq!(plan.nodes[0].node_type, "Filter: (t.s = '(cost=1')");
    assert_eq!(plan.nodes[0].total_cost, None);
    assert_eq!(plan.nodes[1].node_type, "Filter: (t.a > 5)");

    let one = "-> <not executable by iterator executor>\n";
    let plan = ExplainOutput::from_mysql_tree(one.to_owned()).plan.unwrap();
    assert_eq!(plan.nodes.len(), 1);
    assert_eq!(
        plan.nodes[0].node_type,
        "<not executable by iterator executor>"
    );
}

#[test]
fn text_outside_the_grammar_keeps_the_raw_text() {
    for bad in [
        "",
        "Table scan on t",
        "-> a\n  -> two-space indent",
        "-> a\n        -> two levels down",
        "    -> indented first line",
        "-> a\n    ->",
    ] {
        let out = ExplainOutput::from_mysql_tree(bad.to_owned());
        assert_eq!(out.raw, bad);
        assert!(out.plan.is_err(), "{bad:?}");
    }
}

#[test]
fn every_captured_mysql_analyze_tree_parses_with_a_line_per_node() {
    let all = my_explain_fixtures(&["mysql"], ".txt");
    assert!(all.len() >= 20, "{} MySQL tree fixtures", all.len());
    for (name, raw) in all {
        let plan = ExplainOutput::from_mysql_tree(raw.clone())
            .plan
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(plan.nodes.len(), raw.trim_end().lines().count(), "{name}");
        assert!(plan.analyzed, "{name}");
        // A dependent subquery prints as a later top-level line, so it starts a second root.
        if name.contains("dependent_sleep") {
            let n = &plan.nodes[2];
            assert_eq!(
                (n.depth, n.parent, n.node_type.as_str()),
                (0, None, "Select #2 (subquery in projection; dependent)"),
                "{name}"
            );
        }
    }
}

fn fixture(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/explain/my")
        .join(name);
    std::fs::read_to_string(path).unwrap()
}

fn execution(raw: &str) -> f64 {
    ExplainOutput::from_mysql_tree(raw.to_owned())
        .plan
        .unwrap()
        .execution_ms
        .unwrap()
}

#[test]
fn projection_subquery_roots_run_outside_root_zero_and_count() {
    let ms = execution(&fixture("mysql84-analyze-two_proj.txt"));
    assert!(close(ms, 0.0257 + 0.564 * 200.0 + 0.523 * 200.0), "{ms}");

    let ms = execution(&fixture("mysql84-analyze-dependent_sleep.txt"));
    assert!(close(ms, 0.141 + 0.00248 * 5.0), "{ms}");
}

#[test]
fn a_select_root_that_root_zero_references_is_inside_it_and_not_counted() {
    let ms = execution(&fixture("mysql84-analyze-order_by.txt"));
    assert!(close(ms, 105.0), "{ms}");
}

#[test]
fn an_extra_root_that_is_not_a_select_is_skipped() {
    let raw = "-> Table scan on t  (actual time=0.1..0.5 rows=3 loops=1)
-> Something else
    -> Table scan on u  (actual time=0.1..9 rows=3 loops=1)
";
    assert!(close(execution(raw), 0.5), "only root 0 counts");
}

#[test]
fn a_reference_matches_the_whole_select_number_only() {
    let raw = "-> Filter: (t.a > (select #2))  (actual time=0.1..0.5 rows=3 loops=1)
-> Select #20 (subquery in projection; dependent)
    -> Table scan on u  (actual time=0.1..2 rows=3 loops=3)
";
    assert!(close(execution(raw), 0.5 + 6.0), "#2 must not hide #20");
}

#[test]
fn a_select_root_referencing_itself_still_counts() {
    let raw = "-> Table scan on t  (actual time=0.1..0.5 rows=3 loops=1)
-> Select #2 (subquery in projection; dependent)
    -> Filter: (u.a = (select #2))  (actual time=0.1..2 rows=3 loops=3)
";
    assert!(
        close(execution(raw), 0.5 + 6.0),
        "a self-reference is not outside use"
    );
}

#[test]
fn a_projection_subquery_node_takes_its_share_of_the_whole_run() {
    let plan = ExplainOutput::from_mysql_tree(fixture("mysql84-analyze-two_proj.txt"))
        .plan
        .unwrap();
    let ix = plan
        .nodes
        .iter()
        .position(|n| n.node_type == "Table scan on b")
        .unwrap();
    let share = plan.self_share(ix).unwrap();
    assert!(share < 0.5, "{share}");
}

#[test]
fn a_root_takes_the_time_of_a_slower_node_below_it() {
    // The subquery runs while the optimizer picks the range, so root 0 prints 0.0283 ms.
    let ms = execution(&fixture("mysql84-analyze-where_uncorr.txt"));
    assert!(close(ms, 48.1), "{ms}");
}

#[test]
fn every_other_captured_tree_runs_as_long_as_its_first_root() {
    let skip = ["dependent_sleep", "two_proj", "where_uncorr"];
    let mut checked = 0;
    for (name, raw) in my_explain_fixtures(&["mysql"], ".txt") {
        if skip.iter().any(|s| name.contains(&format!("-{s}."))) {
            continue;
        }
        let plan = ExplainOutput::from_mysql_tree(raw).plan.unwrap();
        let root = plan.nodes[0].actual.unwrap();
        let ms = plan.execution_ms.unwrap();
        assert!(close(ms, root.total_ms * root.loops as f64), "{name}: {ms}");
        checked += 1;
    }
    assert!(checked >= 20, "only {checked} MySQL trees checked");
}

#[test]
fn an_upper_case_select_reference_still_hides_its_root() {
    let raw = "-> Sort: (SELECT #2) DESC  (actual time=0.1..5 rows=3 loops=1)
-> Select #2 (subquery in projection; dependent)
    -> Table scan on u  (actual time=0.1..1 rows=3 loops=3)
";
    assert!(close(execution(raw), 5.0), "the sort already ran #2");
}
