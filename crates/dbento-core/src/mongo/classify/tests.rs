use super::mongo_classification;
use crate::mongo::parse::parse_script;
use crate::session::{Unguarded, Writes};

fn class(src: &str) -> (Writes, Vec<Unguarded>) {
    let mut all = parse_script(src);
    assert_eq!(all.len(), 1, "{src}");
    let s = all
        .remove(0)
        .unwrap_or_else(|e| panic!("{src}: {}", e.message));
    let c = mongo_classification(&s.op);
    assert!(!c.transaction_control, "{src}");
    (c.writes, c.unguarded)
}

fn writes(src: &str) -> Writes {
    class(src).0
}

#[test]
fn shell_reads_and_use_do_not_write() {
    for src in [
        "use shop",
        "db.t.find({a: 1}).sort({a: 1})",
        "db.t.findOne()",
        "db.t.aggregate([{$match: {}}, {$group: {_id: '$a'}}])",
        "db.t.countDocuments()",
        "db.t.estimatedDocumentCount()",
        "db.t.distinct('a')",
        "db.t.getIndexes()",
        "db.getCollectionNames()",
        "db.stats()",
    ] {
        assert_eq!(class(src), (Writes::No, vec![]), "{src}");
    }
}

#[test]
fn read_commands_match_their_first_key_case_sensitively() {
    for name in [
        "find",
        "aggregate",
        "count",
        "distinct",
        "listCollections",
        "listIndexes",
        "listDatabases",
        "dbStats",
        "collStats",
        "buildInfo",
        "hello",
        "isMaster",
        "ping",
        "connectionStatus",
        "serverStatus",
        "hostInfo",
        "getParameter",
        "explain",
    ] {
        assert_eq!(
            writes(&format!("db.runCommand({{{name}: 1}})")),
            Writes::No,
            "{name}"
        );
    }
    for src in [
        "db.runCommand({Find: 't'})",
        "db.runCommand({listcollections: 1})",
        "db.runCommand({})",
        "db.runCommand({killOp: 1, op: 5})",
        "db.runCommand({shutdown: 1})",
        "db.runCommand({eval: 'x'})",
    ] {
        assert_eq!(writes(src), Writes::Unknown, "{src}");
    }
}

#[test]
fn an_out_or_merge_stage_at_any_depth_makes_an_aggregate_write() {
    for src in [
        "db.t.aggregate([{$match: {}}, {$out: 'x'}])",
        "db.t.aggregate([{$merge: {into: 'x'}}])",
        "db.t.aggregate([{$facet: {a: [{$match: {}}], b: [{$merge: 'x'}]}}])",
        "db.t.aggregate([{$lookup: {from: 'u', pipeline: [{$out: 'x'}], as: 'u'}}])",
        "db.t.aggregate([{$unionWith: {coll: 'u', pipeline: [{$facet: {z: [{$out: 'x'}]}}]}}])",
        "db.runCommand({aggregate: 't', pipeline: [{$out: 'x'}], cursor: {}})",
    ] {
        assert_eq!(writes(src), Writes::Yes, "{src}");
    }
}

#[test]
fn write_methods_and_commands_write() {
    for src in [
        "db.t.insertOne({})",
        "db.t.insertMany([{}])",
        "db.t.updateOne({}, {$set: {a: 1}})",
        "db.t.updateMany({a: 1}, {$set: {a: 1}})",
        "db.t.replaceOne({}, {})",
        "db.t.deleteOne({})",
        "db.t.deleteMany({a: 1})",
        "db.t.findOneAndUpdate({}, {$set: {a: 1}})",
        "db.t.findOneAndReplace({}, {})",
        "db.t.findOneAndDelete({})",
        "db.t.createIndex({a: 1})",
        "db.t.dropIndex('a_1')",
        "db.t.drop()",
        "db.createCollection('c')",
        "db.dropDatabase()",
    ] {
        assert_eq!(writes(src), Writes::Yes, "{src}");
    }
    for name in [
        "insert",
        "update",
        "delete",
        "findAndModify",
        "create",
        "drop",
        "dropDatabase",
        "createIndexes",
        "dropIndexes",
        "renameCollection",
        "collMod",
        "mapReduce",
    ] {
        assert_eq!(
            writes(&format!("db.runCommand({{{name}: 't'}})")),
            Writes::Yes,
            "{name}"
        );
    }
}

#[test]
fn empty_filters_on_many_document_writes_and_drops_are_unguarded() {
    use Unguarded::*;
    for (src, want) in [
        (
            "db.t.updateMany({}, {$set: {a: 1}})",
            vec![UpdateAllDocuments],
        ),
        ("db.t.updateMany({a: 1}, {$set: {a: 1}})", vec![]),
        ("db.t.updateOne({}, {$set: {a: 1}})", vec![]),
        ("db.t.deleteMany({})", vec![DeleteAllDocuments]),
        ("db.t.deleteMany()", vec![DeleteAllDocuments]),
        ("db.t.deleteOne({})", vec![]),
        ("db.t.findOneAndDelete({})", vec![]),
        ("db.t.drop()", vec![Drop]),
        ("db.dropDatabase()", vec![Drop]),
        (
            "db.runCommand({update: 't', updates: [{q: {}, u: {$set: {a: 1}}, multi: true}]})",
            vec![UpdateAllDocuments],
        ),
        (
            "db.runCommand({update: 't', updates: [{u: {}, multi: true}]})",
            vec![UpdateAllDocuments],
        ),
        (
            "db.runCommand({update: 't', updates: [{q: {}, u: {}, multi: false}]})",
            vec![],
        ),
        (
            "db.runCommand({update: 't', updates: [{q: {}, u: {}}]})",
            vec![],
        ),
        (
            "db.runCommand({update: 't', updates: [{q: {}, u: {}, multi: true}, {q: {a: 1}, u: {}, multi: true}, {q: {}, u: {}, multi: true}]})",
            vec![UpdateAllDocuments, UpdateAllDocuments],
        ),
        (
            "db.runCommand({delete: 't', deletes: [{q: {}, limit: 0}]})",
            vec![DeleteAllDocuments],
        ),
        (
            "db.runCommand({delete: 't', deletes: [{q: {}, limit: NumberLong(0)}]})",
            vec![DeleteAllDocuments],
        ),
        (
            "db.runCommand({delete: 't', deletes: [{q: {}, limit: 1}]})",
            vec![],
        ),
        (
            "db.runCommand({delete: 't', deletes: [{q: {}, limit: NumberDecimal('0')}]})",
            vec![DeleteAllDocuments],
        ),
        (
            "db.runCommand({delete: 't', deletes: [{q: {}, limit: NumberDecimal('0E+3')}]})",
            vec![DeleteAllDocuments],
        ),
        (
            "db.runCommand({delete: 't', deletes: [{q: {}, limit: NumberDecimal('0.00')}]})",
            vec![DeleteAllDocuments],
        ),
        (
            "db.runCommand({delete: 't', deletes: [{q: {}, limit: true}]})",
            vec![DeleteAllDocuments],
        ),
        (
            "db.runCommand({delete: 't', deletes: [{q: {}, limit: false}]})",
            vec![DeleteAllDocuments],
        ),
        (
            "db.runCommand({delete: 't', deletes: [{q: {}, limit: null}]})",
            vec![DeleteAllDocuments],
        ),
        (
            "db.runCommand({delete: 't', deletes: [{q: {}, limit: 'x'}]})",
            vec![DeleteAllDocuments],
        ),
        (
            "db.runCommand({delete: 't', deletes: [{q: {}, limit: {}}]})",
            vec![DeleteAllDocuments],
        ),
        (
            "db.runCommand({delete: 't', deletes: [{q: {}, limit: []}]})",
            vec![DeleteAllDocuments],
        ),
        (
            "db.runCommand({delete: 't', deletes: [{q: {}, limit: 1.0}]})",
            vec![],
        ),
        (
            "db.runCommand({delete: 't', deletes: [{q: {}, limit: NumberLong(1)}]})",
            vec![],
        ),
        (
            "db.runCommand({delete: 't', deletes: [{q: {}, limit: NumberDecimal('1')}]})",
            vec![],
        ),
        ("db.runCommand({delete: 't', deletes: [{q: {}}]})", vec![]),
        (
            "db.runCommand({delete: 't', deletes: [{q: {a: 1}, limit: 0}, {q: {}, limit: 0}, {q: {}, limit: 1}]})",
            vec![DeleteAllDocuments],
        ),
        ("db.runCommand({drop: 't'})", vec![Drop]),
        ("db.runCommand({dropDatabase: 1})", vec![Drop]),
    ] {
        assert_eq!(class(src).1, want, "{src}");
    }
}
