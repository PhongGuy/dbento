//! Write detection for parsed MongoDB statements.
use bson::{Bson, Document};

use super::parse::MongoOp;
use crate::session::{Classification, Unguarded, Writes};

// Server command names, matched case-sensitively as the server does.
const READS: [&str; 18] = [
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
];
const WRITES: [&str; 12] = [
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
];

/// What `op` does, known before it runs; `transaction_control` is always false.
pub(crate) fn mongo_classification(op: &MongoOp) -> Classification {
    // Every method is already built into its server command, so methods and
    // `runCommand` share one first-key judgement.
    let command = match op {
        MongoOp::Use(_) => return classification(Writes::No, vec![]),
        MongoOp::Method { command, .. } | MongoOp::RunCommand(command) => command,
    };
    let Some(name) = command.keys().next().map(String::as_str) else {
        return classification(Writes::Unknown, vec![]);
    };
    let writes = if name == "aggregate" {
        if command.get("pipeline").is_some_and(holds_out_or_merge) {
            Writes::Yes
        } else {
            Writes::No
        }
    } else if READS.contains(&name) {
        Writes::No
    } else if WRITES.contains(&name) {
        Writes::Yes
    } else {
        Writes::Unknown
    };
    classification(writes, unguarded(name, command))
}

fn classification(writes: Writes, unguarded: Vec<Unguarded>) -> Classification {
    Classification {
        writes,
        unguarded,
        transaction_control: false,
    }
}

// A `$`-prefixed key can never name a field, so `$out` or `$merge` anywhere in
// the pipeline is a stage; a false positive only asks for confirmation.
fn holds_out_or_merge(value: &Bson) -> bool {
    match value {
        Bson::Document(d) => d
            .iter()
            .any(|(k, v)| k == "$out" || k == "$merge" || holds_out_or_merge(v)),
        Bson::Array(items) => items.iter().any(holds_out_or_merge),
        _ => false,
    }
}

// One entry per many-document statement with an empty or missing filter, in
// statement order, plus `Drop` for the two drop commands.
fn unguarded(name: &str, command: &Document) -> Vec<Unguarded> {
    let statements = |key| match command.get(key) {
        Some(Bson::Array(items)) => items.iter().filter_map(Bson::as_document).collect(),
        _ => Vec::new(),
    };
    match name {
        "update" => statements("updates")
            .into_iter()
            .filter(|s| empty_filter(s) && s.get("multi") == Some(&Bson::Boolean(true)))
            .map(|_| Unguarded::UpdateAllDocuments)
            .collect(),
        "delete" => statements("deletes")
            .into_iter()
            .filter(|s| empty_filter(s) && s.get("limit").is_some_and(|l| !is_nonzero_number(l)))
            .map(|_| Unguarded::DeleteAllDocuments)
            .collect(),
        "drop" | "dropDatabase" => vec![Unguarded::Drop],
        _ => vec![],
    }
}

fn empty_filter(statement: &Document) -> bool {
    match statement.get("q") {
        None => true,
        Some(Bson::Document(q)) => q.is_empty(),
        Some(_) => false,
    }
}

// The server deletes one document only for a non-zero number; every other
// `limit` type, however odd, deletes them all.
fn is_nonzero_number(value: &Bson) -> bool {
    match value {
        Bson::Int32(n) => *n != 0,
        Bson::Int64(n) => *n != 0,
        Bson::Double(n) => *n != 0.0,
        Bson::Decimal128(d) => d.to_string().parse::<f64>() != Ok(0.0),
        _ => false,
    }
}

#[cfg(test)]
mod tests;
