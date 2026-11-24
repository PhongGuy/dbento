use std::collections::BTreeSet;

use super::S3Op;

const EVERY_OP: [S3Op; 4] = [
    S3Op::ListBuckets,
    S3Op::ListObjectsV2,
    S3Op::HeadObject,
    S3Op::GetObject,
];

#[test]
fn the_operation_set_is_closed_to_four_reads() {
    // No wildcard: a new variant fails to compile here before the client can send it.
    let names: BTreeSet<&str> = EVERY_OP
        .into_iter()
        .map(|op| match op {
            S3Op::ListBuckets | S3Op::ListObjectsV2 | S3Op::HeadObject | S3Op::GetObject => {
                op.name()
            }
        })
        .collect();
    assert_eq!(
        names,
        BTreeSet::from(["GetObject", "HeadObject", "ListBuckets", "ListObjectsV2"])
    );
}

#[test]
fn every_operation_needs_only_a_reader_iam_action() {
    let actions: BTreeSet<&str> = EVERY_OP.into_iter().map(S3Op::iam_action).collect();
    assert_eq!(
        actions,
        BTreeSet::from(["s3:GetObject", "s3:ListAllMyBuckets", "s3:ListBucket"])
    );
    assert_eq!(S3Op::ListObjectsV2.iam_action(), "s3:ListBucket");
    assert_eq!(S3Op::HeadObject.iam_action(), "s3:GetObject");
}

#[test]
fn the_reader_policy_text_names_every_iam_action() {
    let residual = crate::read_only_residual(crate::Engine::S3).unwrap();
    let help = crate::connection_form(crate::Engine::S3)
        .read_only_help
        .unwrap();
    for op in EVERY_OP {
        assert!(residual.contains(op.iam_action()), "{op:?}: {residual}");
        assert!(help.contains(op.iam_action()), "{op:?}: {help}");
    }
}
