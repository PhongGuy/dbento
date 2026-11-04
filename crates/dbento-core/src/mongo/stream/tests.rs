use bson::doc;

use super::*;
use crate::mongo::parse::parse_script;

fn op(src: &str) -> MongoOp {
    parse_script(src).remove(0).unwrap().op
}

fn at_most(rows: u64) -> Limits {
    Limits {
        max_rows: Some(rows),
        ..Limits::default()
    }
}

#[test]
fn the_listed_shapes_are_cursors_and_an_explain_is_not() {
    for src in [
        "db.t.find()",
        "db.t.findOne()",
        "db.t.aggregate([])",
        "db.t.getIndexes()",
        "db.getCollectionNames()",
        "db.runCommand({find: 't'})",
        "db.runCommand({aggregate: 't', pipeline: [], cursor: {}})",
        "db.runCommand({listCollections: 1})",
        "db.runCommand({listIndexes: 't'})",
    ] {
        assert!(is_cursor(&op(src)), "{src}");
    }
    for src in [
        "use other",
        "db.t.insertOne({})",
        "db.t.countDocuments()",
        "db.t.aggregate([], {explain: true})",
        "db.runCommand({aggregate: 't', pipeline: [], explain: true})",
        "db.runCommand({ping: 1})",
        // Only the first key names a command.
        "db.runCommand({ping: 1, find: 't'})",
    ] {
        assert!(!is_cursor(&op(src)), "{src}");
    }
}

#[test]
fn a_small_row_limit_sets_batch_size_and_nothing_else() {
    let find = with_batch_size(doc! { "find": "t", "limit": 3 }, at_most(3));
    assert_eq!(find, doc! { "find": "t", "limit": 3, "batchSize": 4 });
    let aggregate = doc! { "aggregate": "t", "pipeline": [], "cursor": {} };
    let want = doc! { "aggregate": "t", "pipeline": [], "cursor": { "batchSize": 100 } };
    assert_eq!(with_batch_size(aggregate, at_most(99)), want);
}

#[test]
fn batch_size_stays_out_when_it_could_not_help_or_the_user_gave_one() {
    let unchanged = [
        (doc! { "find": "t" }, Limits::default()),
        (doc! { "find": "t" }, at_most(100)),
        (doc! { "find": "t" }, at_most(u64::MAX)),
        (doc! { "find": "t", "batchSize": 7 }, at_most(3)),
        // On `singleBatch` a `batchSize` caps the whole result, not just its first reply.
        (
            doc! { "find": "t", "limit": 50, "singleBatch": true },
            at_most(3),
        ),
        (
            doc! { "aggregate": "t", "cursor": { "batchSize": 7 } },
            at_most(3),
        ),
        // The server refuses an aggregate with no `cursor`, and that stays its answer.
        (doc! { "aggregate": "t", "pipeline": [] }, at_most(3)),
        (doc! { "listIndexes": "t", "cursor": {} }, at_most(3)),
        (doc! { "listCollections": 1, "nameOnly": true }, at_most(3)),
    ];
    for (command, limits) in unchanged {
        assert_eq!(with_batch_size(command.clone(), limits), command);
    }
}
