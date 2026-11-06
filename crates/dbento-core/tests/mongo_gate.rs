//! The script and the read-only gate: a refused statement never reaches the server.
#![cfg(feature = "mongodb")]
mod support;

use dbento_core::{ExecuteError, Limits, ReadOnlyEnforcement, SessionOptions};
use dbento_testkit::MgUser::{Reader, Writer};
use support::{failure, forced, mg, read, run};

const WRITES: [&str; 13] = [
    "db.many.insertOne({a: 1})",
    "db.many.updateOne({_id: 1}, {$set: {a: 1}})",
    "db.many.deleteOne({_id: 1})",
    "db.many.drop()",
    "db.many.createIndex({a: 1})",
    "db.createCollection('c')",
    "db.many.aggregate([{$out: 'c'}])",
    "db.many.aggregate([{$facet: {a: [{$merge: 'c'}]}}])",
    "db.runCommand({insert: 'many', documents: [{a: 1}]})",
    "db.dropDatabase()",
    "db.many.findOneAndUpdate({_id: 1}, {$set: {a: 1}})",
    "db.many.findOneAndDelete({_id: 1})",
    // Options go on top of the count's own command, so this one replaces its pipeline.
    "db.many.countDocuments({}, {pipeline: [{$out: 'c'}]})",
];

#[tokio::test]
async fn a_reader_is_refused_every_write_before_it_is_sent() {
    let fx = mg("").await;
    let mut s = fx.session(Reader, forced()).await;
    for statement in WRITES {
        // The gate's own words: a server refusal maps to this variant too, with its message.
        match failure(&mut s, statement).await {
            ExecuteError::ReadOnlyViolation { message } => {
                assert_eq!(message, "this statement writes", "{statement}");
            }
            other => panic!("{statement}: {other:?}"),
        }
    }
    assert_eq!(fx.db.count("many").await, 10_000);
}

#[tokio::test]
async fn a_writer_forced_read_only_is_refused_writes_and_unknown_commands() {
    let fx = mg("").await;
    let mut s = fx.session(Writer, forced()).await;
    let unknown = [
        "db.runCommand({setParameter: 1, x: 1})",
        "db.runCommand({})",
    ];
    for statement in WRITES.into_iter().chain(unknown) {
        assert!(matches!(
            failure(&mut s, statement).await,
            ExecuteError::ReadOnlyViolation { .. }
        ));
    }
    assert_eq!(fx.db.count("many").await, 10_000);
}

// The one `Writes::No` path that wraps a write, and only the server keeps it from writing, so a
// server upgrade that changes this has to fail here.
#[tokio::test]
async fn an_explain_wrapping_a_write_passes_the_gate_and_writes_nothing() {
    let fx = mg("").await;
    let mut s = fx.session(Writer, forced()).await;
    let explain = "db.runCommand({explain: {aggregate: 'many', pipeline: [{$out: 'c'}], \
                   cursor: {}}, verbosity: 'executionStats'})";
    let ran = read(&mut s, explain).await;
    assert_eq!(ran.rows.len(), 1);
    assert_eq!(fx.db.count("c").await, 0);
}

#[tokio::test]
async fn a_command_the_gate_cannot_read_is_refused_by_the_server_for_a_reader() {
    let fx = mg("").await;
    let mut s = fx.session(Reader, forced()).await;
    // Only under `Database` does the gate let an unknown command through to the server.
    assert_eq!(
        s.read_only_enforcement(),
        Some(ReadOnlyEnforcement::Database)
    );
    let bulk = format!(
        "db.getSiblingDB('admin').runCommand({{bulkWrite: 1, \
         ops: [{{insert: 0, document: {{a: 1}}}}], nsInfo: [{{ns: '{}.many'}}]}})",
        fx.db.database()
    );
    match failure(&mut s, &bulk).await {
        // The server's 13 carries its own words, unlike either of the gate's refusals.
        ExecuteError::ReadOnlyViolation { message } => {
            assert!(message.contains("not authorized"), "{message}");
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(fx.db.count("many").await, 10_000);
}

#[tokio::test]
async fn a_script_prepares_each_statement_and_use_changes_settings() {
    let fx = mg("").await;
    let mut s = fx.session(Reader, SessionOptions::default()).await;
    assert!(!s.settings_changed());
    {
        let mut script = s.script("use other;\ndb.many.find()");
        let mut first = script.next().await.unwrap().unwrap();
        assert_eq!(first.sql(), "use other;");
        assert!(!first.is_last().await);
        first.execute(Limits::default()).await.unwrap();
        let mut second = script.next().await.unwrap().unwrap();
        assert_eq!(second.sql(), "db.many.find()");
        assert!(second.is_last().await);
        second.skip();
        assert!(script.next().await.is_none());
    }
    assert!(s.settings_changed());
    assert_eq!(s.current_database().await.as_deref(), Some("other"));
}

#[tokio::test]
async fn a_syntax_error_ends_the_script_before_the_next_statement() {
    let fx = mg("").await;
    let mut s = fx.session(Writer, SessionOptions::default()).await;
    let mut script = s.script("db.many.find(x)\ndb.many.deleteMany({})");
    assert!(matches!(
        script.next().await,
        Some(Err(ExecuteError::Syntax { .. }))
    ));
    assert!(script.next().await.is_none());
}

#[tokio::test]
async fn a_syntax_error_reports_its_offset_inside_the_statement() {
    let fx = mg("").await;
    let mut s = fx.session(Reader, SessionOptions::default()).await;
    let mut ran = run(&mut s, "use app\ndb.many.find(x)", Limits::default()).await;
    match ran.remove(1) {
        Err(ExecuteError::Syntax { message, offset }) => {
            assert_eq!(offset, 13);
            assert_eq!(
                message,
                "Dbento runs MongoDB shell statements, not JavaScript"
            );
        }
        other => panic!("{:?}", other.map(|r| r.outcome)),
    }
}
