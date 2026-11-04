use super::{MongoMethod, MongoOp, MongoStatement, SyntaxError, parse_script};
use crate::mongo::split::statement_ranges;
use crate::mongo::value::NOT_JS;
use bson::{Document, doc};

fn one(src: &str) -> MongoStatement {
    let mut all = parse_script(src);
    assert_eq!(all.len(), 1, "{src}");
    all.remove(0)
        .unwrap_or_else(|e| panic!("{src}: {}", e.message))
}

fn cmd(src: &str) -> (MongoMethod, Document) {
    match one(src).op {
        MongoOp::Method { method, command } => (method, command),
        other => panic!("{src}: {other:?}"),
    }
}

fn fail(src: &str) -> SyntaxError {
    let mut all = parse_script(src);
    assert_eq!(all.len(), 1, "{src}");
    all.remove(0)
        .err()
        .unwrap_or_else(|| panic!("{src} parsed"))
}

#[test]
fn reads_build_one_server_command_each() {
    use MongoMethod::*;
    let group = doc! { "$group": { "_id": 1, "n": { "$sum": 1 } } };
    for (src, method, want) in [
        (
            "db.orders.find()",
            Find,
            doc! { "find": "orders", "filter": {} },
        ),
        (
            "db.orders.find({status: 'open'}, {_id: 0}).sort({_id: -1}).limit(5).skip(1)",
            Find,
            doc! { "find": "orders", "filter": { "status": "open" }, "projection": { "_id": 0 },
            "sort": { "_id": -1 }, "limit": 5, "skip": 1 },
        ),
        (
            "db.t.find({}).hint('a_1').collation({locale: 'en'}).comment('c').batchSize(10).projection({a: 1}).limit(1).limit(2)",
            Find,
            doc! { "find": "t", "filter": {}, "hint": "a_1", "collation": { "locale": "en" },
            "comment": "c", "batchSize": 10, "projection": { "a": 1 }, "limit": 2 },
        ),
        (
            "db.t.findOne({a: 1}, {b: 1})",
            FindOne,
            doc! { "find": "t", "filter": { "a": 1 }, "projection": { "b": 1 }, "limit": 1, "singleBatch": true },
        ),
        (
            "db.t.aggregate([{$match: {a: 1}}], {allowDiskUse: true, batchSize: 5})",
            Aggregate,
            doc! { "aggregate": "t", "pipeline": [{ "$match": { "a": 1 } }],
            "cursor": { "batchSize": 5 }, "allowDiskUse": true },
        ),
        (
            "db.t.aggregate([])",
            Aggregate,
            doc! { "aggregate": "t", "pipeline": [], "cursor": {} },
        ),
        (
            "db.t.aggregate([], {cursor: {batchSize: 2}, allowDiskUse: true})",
            Aggregate,
            doc! { "aggregate": "t", "pipeline": [], "cursor": { "batchSize": 2 }, "allowDiskUse": true },
        ),
        (
            "db.t.aggregate([], {cursor: {batchSize: 2}, batchSize: 5})",
            Aggregate,
            doc! { "aggregate": "t", "pipeline": [], "cursor": { "batchSize": 5 } },
        ),
        (
            "db.t.find().limit(-3)",
            Find,
            doc! { "find": "t", "filter": {}, "limit": 3, "singleBatch": true },
        ),
        (
            "db.t.countDocuments({a: 1}, {skip: 1, limit: 2})",
            CountDocuments,
            doc! { "aggregate": "t",
            "pipeline": [{ "$match": { "a": 1 } }, { "$skip": 1 }, { "$limit": 2 }, group.clone()],
            "cursor": {} },
        ),
        (
            "db.t.countDocuments()",
            CountDocuments,
            doc! { "aggregate": "t", "pipeline": [{ "$match": {} }, group], "cursor": {} },
        ),
        (
            "db.t.estimatedDocumentCount()",
            EstimatedDocumentCount,
            doc! { "count": "t" },
        ),
        (
            "db.t.distinct('a', {b: 1})",
            Distinct,
            doc! { "distinct": "t", "key": "a", "query": { "b": 1 } },
        ),
        (
            "db.t.distinct('a')",
            Distinct,
            doc! { "distinct": "t", "key": "a" },
        ),
        (
            "db.t.getIndexes()",
            GetIndexes,
            doc! { "listIndexes": "t", "cursor": {} },
        ),
        (
            "db.getCollectionNames()",
            GetCollectionNames,
            doc! { "listCollections": 1, "nameOnly": true, "authorizedCollections": true },
        ),
        ("db.stats()", Stats, doc! { "dbStats": 1 }),
    ] {
        let (m, d) = cmd(src);
        assert_eq!((m, d), (method, want), "{src}");
    }
    assert_eq!(
        one("db.runCommand({ping: 1})").op,
        MongoOp::RunCommand(doc! { "ping": 1 })
    );
}
#[test]
fn the_target_collection_and_database_take_every_shell_form() {
    for (src, db, coll) in [
        ("db['a.b'].find()", None, "a.b"),
        ("db.getCollection('x y').find()", None, "x y"),
        ("db.system.profile.find()", None, "system.profile"),
        ("db.getCollection('a').b.find()", None, "a.b"),
        ("db\n  .t\n  .find()", None, "t"),
        ("db.getSiblingDB('other').t.find()", Some("other"), "t"),
    ] {
        assert_eq!(one(src).db.as_deref(), db, "{src}");
        assert_eq!(cmd(src).1.get_str("find").unwrap(), coll, "{src}");
    }
    let s = one("db.getSiblingDB('admin').runCommand({ping: 1})");
    assert_eq!(
        (s.db.as_deref(), s.op),
        (Some("admin"), MongoOp::RunCommand(doc! { "ping": 1 }))
    );
    assert_eq!(one("use shop").op, MongoOp::Use("shop".into()));
    assert_eq!(one("use my-db;").op, MongoOp::Use("my-db".into()));
}

#[test]
fn a_script_parses_each_statement_over_the_splitters_ranges() {
    let src = "use shop\ndb.t.find()\n;db.t.countDocuments({a: 1}); db.t.foo()";
    let all = parse_script(src);
    let ranges: Vec<_> = all
        .iter()
        .map(|r| {
            r.as_ref()
                .map_or_else(|e| e.range.clone(), |s| s.range.clone())
        })
        .collect();
    assert_eq!((all.len(), ranges), (4, statement_ranges(src)));
    assert!(matches!(
        all[1],
        Ok(MongoStatement {
            op: MongoOp::Method {
                method: MongoMethod::Find,
                ..
            },
            ..
        })
    ));
    let e = all[3].as_ref().unwrap_err();
    assert_eq!(
        (e.message.as_str(), e.offset),
        ("`foo` is not a supported collection method", 5)
    );
}

#[test]
fn statements_outside_the_subset_are_refused_with_what_went_wrong() {
    for (src, want) in [
        ("db.t.foo()", "`foo` is not a supported collection method"),
        ("db.find()", "`find` is not a supported database method"),
        (
            "db.getIndexes()",
            "`getIndexes` is not a supported database method",
        ),
        (
            "db.t.stats()",
            "`stats` is not a supported collection method",
        ),
        ("db.t", "expected a method call"),
        ("db.t.find().count()", "`.count` cannot follow `find`"),
        (
            "db.t.findOne({}).sort({})",
            "`.sort` cannot follow `findOne`",
        ),
        ("db.t.find() db.u.find()", "expected `;` or a new line"),
        ("db.t.distinct()", "`distinct` needs an argument"),
        ("db.runCommand()", "`runCommand` needs an argument"),
        ("db.t.find(1)", "argument 1 of `find` must be a document"),
        (
            "db.t.aggregate({})",
            "argument 1 of `aggregate` must be an array",
        ),
        (
            "db.t.distinct(1)",
            "argument 1 of `distinct` must be a string",
        ),
        ("db.t.getIndexes(1)", "`getIndexes` takes no arguments"),
        ("db.t.find({}, {}, {})", "`find` takes at most 2 arguments"),
        (
            "db.t.find().limit('x')",
            "argument 1 of `.limit` must be a number",
        ),
        (
            "db.t.find().hint(1)",
            "argument 1 of `.hint` must be a string or a document",
        ),
        ("db.t.find().sort()", "`.sort` needs an argument"),
        ("use", "`use` needs a database name"),
        ("use a b", "`a b` is not a database name"),
        ("use a.b", "`a.b` is not a database name"),
        ("db.t.find({s: 'open", "unterminated string"),
        ("x = 1", NOT_JS),
        ("const a = 1", NOT_JS),
        ("print('x')", NOT_JS),
        ("show dbs", NOT_JS),
        ("db = 1", NOT_JS),
        ("db.t.find(filter)", NOT_JS),
        ("db.t.find({}).limit(n)", NOT_JS),
    ] {
        assert_eq!(fail(src).message, want, "{src}");
    }
}

#[test]
fn an_error_offset_counts_from_its_statements_start() {
    let src = "db.a.find();\ndb.t.find({a: b})";
    let e = parse_script(src).pop().unwrap().unwrap_err();
    assert_eq!(
        (e.offset, e.range.clone(), e.message.as_str()),
        (14, 13..src.len(), NOT_JS)
    );
    assert_eq!(fail("db.t.find({s: 'open").offset, 14);
}

#[test]
fn a_statement_starting_with_punctuation_or_a_number_is_not_js_at_its_own_start() {
    for src in ["db.a.find()\n1", "x;(db.t.find())", "db.a.find()\n{}"] {
        let all = parse_script(src);
        let e = all[1].as_ref().unwrap_err();
        assert_eq!((e.message.as_str(), e.offset), (NOT_JS, 0), "{src}");
    }
}

#[test]
fn writes_build_their_server_commands() {
    assert_eq!(
        cmd("db.t.insertOne({a: 1})"),
        (
            MongoMethod::InsertOne,
            doc! { "insert": "t", "documents": [{ "a": 1 }] }
        )
    );
    assert_eq!(
        cmd("db.t.insertMany([{a: 1}, {a: 2}], {ordered: false})"),
        (
            MongoMethod::InsertMany,
            doc! {
                "insert": "t", "documents": [{ "a": 1 }, { "a": 2 }], "ordered": false
            }
        )
    );
    assert_eq!(
        cmd("db.t.updateOne({a: 1}, {$set: {b: 2}}, {upsert: true, comment: 'c'})"),
        (
            MongoMethod::UpdateOne,
            doc! {
                "update": "t",
                "updates": [{ "q": { "a": 1 }, "u": { "$set": { "b": 2 } }, "multi": false, "upsert": true }],
                "comment": "c"
            }
        )
    );
    assert_eq!(
        cmd("db.t.updateMany({}, [{$set: {c: 1}}])"),
        (
            MongoMethod::UpdateMany,
            doc! {
                "update": "t", "updates": [{ "q": {}, "u": [{ "$set": { "c": 1 } }], "multi": true }]
            }
        )
    );
    assert_eq!(
        cmd("db.t.replaceOne({a: 1}, {a: 2})"),
        (
            MongoMethod::ReplaceOne,
            doc! {
                "update": "t", "updates": [{ "q": { "a": 1 }, "u": { "a": 2 }, "multi": false }]
            }
        )
    );
    assert_eq!(
        cmd("db.t.deleteOne({a: 1}, {hint: 'a_1'})"),
        (
            MongoMethod::DeleteOne,
            doc! {
                "delete": "t", "deletes": [{ "q": { "a": 1 }, "limit": 1, "hint": "a_1" }]
            }
        )
    );
    assert_eq!(
        cmd("db.t.deleteMany()"),
        (
            MongoMethod::DeleteMany,
            doc! { "delete": "t", "deletes": [{ "q": {}, "limit": 0 }] }
        )
    );
    assert_eq!(
        cmd(
            "db.t.findOneAndUpdate({a: 1}, {$inc: {n: 1}}, {returnDocument: 'after', projection: {n: 1}, sort: {a: 1}, upsert: true})"
        ),
        (
            MongoMethod::FindOneAndUpdate,
            doc! {
                "findAndModify": "t", "query": { "a": 1 }, "update": { "$inc": { "n": 1 } },
                "new": true, "fields": { "n": 1 }, "sort": { "a": 1 }, "upsert": true
            }
        )
    );
    assert_eq!(
        cmd("db.t.findOneAndReplace({a: 1}, {a: 2}, {returnNewDocument: false})"),
        (
            MongoMethod::FindOneAndReplace,
            doc! {
                "findAndModify": "t", "query": { "a": 1 }, "update": { "a": 2 }, "new": false
            }
        )
    );
    assert_eq!(
        cmd("db.t.findOneAndDelete({a: 1}, {sort: {a: 1}})"),
        (
            MongoMethod::FindOneAndDelete,
            doc! {
                "findAndModify": "t", "query": { "a": 1 }, "remove": true, "sort": { "a": 1 }
            }
        )
    );
}

#[test]
fn ddl_and_database_writes_build_their_server_commands() {
    assert_eq!(
        cmd("db.t.createIndex({a: 1, b: -1}, {unique: true})"),
        (
            MongoMethod::CreateIndex,
            doc! {
                "createIndexes": "t",
                "indexes": [{ "key": { "a": 1, "b": -1 }, "name": "a_1_b_-1", "unique": true }]
            }
        )
    );
    assert_eq!(
        cmd("db.t.createIndex({loc: '2dsphere'}, {name: 'geo'})").1,
        doc! { "createIndexes": "t", "indexes": [{ "key": { "loc": "2dsphere" }, "name": "geo" }] }
    );
    assert_eq!(
        cmd("db.t.dropIndex('a_1')"),
        (
            MongoMethod::DropIndex,
            doc! { "dropIndexes": "t", "index": "a_1" }
        )
    );
    assert_eq!(
        cmd("db.t.dropIndex({a: 1})").1,
        doc! { "dropIndexes": "t", "index": { "a": 1 } }
    );
    assert_eq!(
        cmd("db.t.drop()"),
        (MongoMethod::Drop, doc! { "drop": "t" })
    );
    assert_eq!(
        cmd("db.createCollection('logs', {capped: true, size: 1024})"),
        (
            MongoMethod::CreateCollection,
            doc! { "create": "logs", "capped": true, "size": 1024 }
        )
    );
    assert_eq!(
        cmd("db.dropDatabase()"),
        (MongoMethod::DropDatabase, doc! { "dropDatabase": 1 })
    );
    assert_eq!(
        one("db.getSiblingDB('old').dropDatabase()").db.as_deref(),
        Some("old")
    );
}

#[test]
fn write_arguments_are_checked_against_the_table() {
    for (src, want) in [
        ("db.t.insertOne()", "`insertOne` needs an argument"),
        (
            "db.t.insertOne({a: 1}).sort({})",
            "`.sort` cannot follow `insertOne`",
        ),
        (
            "db.t.insertMany({})",
            "argument 1 of `insertMany` must be an array",
        ),
        ("db.t.updateOne({})", "`updateOne` needs 2 arguments"),
        (
            "db.t.updateOne({}, 1)",
            "argument 2 of `updateOne` must be a document or an array",
        ),
        (
            "db.t.replaceOne({}, [])",
            "argument 2 of `replaceOne` must be a document",
        ),
        ("db.t.deleteOne()", "`deleteOne` needs an argument"),
        (
            "db.t.dropIndex(1)",
            "argument 1 of `dropIndex` must be a string or a document",
        ),
        ("db.t.drop(1)", "`drop` takes no arguments"),
        (
            "db.createCollection(1)",
            "argument 1 of `createCollection` must be a string",
        ),
        (
            "db.t.dropDatabase()",
            "`dropDatabase` is not a supported collection method",
        ),
        (
            "db.t.findOneAndUpdate({}, {}, {returnDocument: 'later'})",
            "returnDocument must be \"before\" or \"after\"",
        ),
        (
            "db.t.updateOne({}, {b: 2})",
            "Update document requires atomic operators",
        ),
        (
            "db.t.updateMany({}, {})",
            "Update document requires atomic operators",
        ),
        (
            "db.t.findOneAndUpdate({}, {b: 1, $set: {c: 1}})",
            "Update document requires atomic operators",
        ),
        (
            "db.t.replaceOne({}, {$set: {a: 1}})",
            "Replacement document must not contain atomic operators",
        ),
        (
            "db.t.findOneAndReplace({}, {a: 1, $inc: {n: 1}})",
            "Replacement document must not contain atomic operators",
        ),
    ] {
        assert_eq!(fail(src).message, want, "{src}");
    }
}

#[test]
fn an_operator_refusal_points_at_the_update_argument() {
    let src = "db.t.updateOne({a: 1}, {b: 2})";
    assert_eq!(fail(src).offset, src.find("{b").unwrap());
    let src = "db.t.findOneAndReplace({}, {$set: {a: 1}}, {sort: {a: 1}})";
    assert_eq!(fail(src).offset, src.find("{$set").unwrap());
}

#[test]
fn a_sort_option_goes_into_the_update_statement() {
    assert_eq!(
        cmd("db.t.updateOne({a: 1}, {$set: {b: 1}}, {sort: {a: -1}, comment: 'c'})").1,
        doc! {
            "update": "t",
            "updates": [{ "q": { "a": 1 }, "u": { "$set": { "b": 1 } }, "multi": false, "sort": { "a": -1 } }],
            "comment": "c"
        }
    );
    assert_eq!(
        cmd("db.t.replaceOne({a: 1}, {a: 2}, {sort: {a: 1}})").1,
        doc! {
            "update": "t",
            "updates": [{ "q": { "a": 1 }, "u": { "a": 2 }, "multi": false, "sort": { "a": 1 } }]
        }
    );
}

#[test]
fn return_document_before_sets_new_false_and_array_filters_join_the_statement() {
    let (_, before) =
        cmd("db.t.findOneAndUpdate({a: 1}, {$inc: {n: 1}}, {returnDocument: 'before'})");
    assert!(!before.get_bool("new").unwrap());
    let (_, updated) = cmd(
        "db.t.updateOne({a: 1}, {$set: {'g.$[e]': 0}}, {arrayFilters: [{e: {$gt: 4}}], upsert: true})",
    );
    let statement = updated.get_array("updates").unwrap()[0]
        .as_document()
        .unwrap();
    assert!(statement.get_array("arrayFilters").is_ok());
    assert!(statement.get_bool("upsert").unwrap());
    assert!(updated.get("arrayFilters").is_none());
}
