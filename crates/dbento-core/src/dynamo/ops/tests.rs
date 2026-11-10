use std::collections::BTreeSet;

use super::{DynamoOp, allowed};
use crate::dynamo::statement::Kind;

const KINDS: [Kind; 5] = [
    Kind::Select,
    Kind::Insert,
    Kind::Update,
    Kind::Delete,
    Kind::Other,
];

fn every_op() -> Vec<DynamoOp> {
    let mut ops = vec![
        DynamoOp::ListTables,
        DynamoOp::DescribeTable,
        DynamoOp::Scan,
    ];
    ops.extend(KINDS.map(DynamoOp::ExecuteStatement));
    ops
}

#[test]
fn the_operation_set_is_closed_to_four_api_calls() {
    // No wildcard: a new variant fails to compile here before the wrapper can send it.
    let names: BTreeSet<&str> = every_op()
        .into_iter()
        .map(|op| match op {
            DynamoOp::ListTables
            | DynamoOp::DescribeTable
            | DynamoOp::Scan
            | DynamoOp::ExecuteStatement(_) => op.name(),
        })
        .collect();
    assert_eq!(
        names,
        BTreeSet::from(["DescribeTable", "ExecuteStatement", "ListTables", "Scan"])
    );
}

#[test]
fn a_writable_session_may_send_every_operation() {
    for op in every_op() {
        assert!(allowed(op, false), "{op:?}");
    }
}

#[test]
fn a_read_only_session_sends_only_the_catalog_calls_and_select() {
    let permitted: Vec<DynamoOp> = every_op()
        .into_iter()
        .filter(|op| allowed(*op, true))
        .collect();
    assert_eq!(
        permitted,
        [
            DynamoOp::ListTables,
            DynamoOp::DescribeTable,
            DynamoOp::Scan,
            DynamoOp::ExecuteStatement(Kind::Select),
        ]
    );
}
