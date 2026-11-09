//! What only a replica set shows: change streams and read preference.
#![cfg(feature = "mongodb")]

use std::time::Duration;

use crate::support::{failure, one, read};
use bson::doc;
use dbento_core::{
    ExecuteError, IntrospectError, Limits, RelationRef, Session, SessionOptions, Workspace, Writes,
    connect,
};
use dbento_testkit::{MgReplicaSet, mongo_replica_set};

/// The set has no secondary, so a read that honours this fails selection, and fails fast.
const SECONDARY: &str = "readPreference=secondary&serverSelectionTimeoutMS=200";

/// A session on `set` with `query` in its URI; the workspace folder lives as long as the session.
async fn session(
    set: &MgReplicaSet,
    query: &str,
    o: SessionOptions,
) -> (tempfile::TempDir, Session) {
    let dir = tempfile::tempdir().unwrap();
    let uri = set.uri(query);
    let toml = format!("dbento = 1\n[connections.set]\nengine = \"mongodb\"\nuri = \"{uri}\"\n");
    std::fs::write(dir.path().join("dbento.toml"), toml).unwrap();
    let ws = Workspace::open(dir.path()).unwrap();
    let conn = ws.connection("set").unwrap();
    let session = connect(&ws, conn, None, o).await.unwrap();
    (dir, session)
}

// A change stream has no end of its own, and only the client-side deadline can stop one. The
// count takes the stream through its `pipeline` option.
#[tokio::test]
async fn a_change_stream_stops_at_the_statement_timeout() {
    let set = mongo_replica_set().await;
    set.run(doc! { "create": "t" }).await;
    let mut o = SessionOptions::default();
    o.statement_timeout = Some(Duration::from_millis(100));
    let (_dir, mut s) = session(&set, "retryReads=false", o).await;
    for statement in [
        "db.t.aggregate([{$changeStream: {}}])",
        "db.t.countDocuments({}, {pipeline: [{$changeStream: {}}]})",
    ] {
        let stopped = tokio::time::timeout(Duration::from_secs(6), failure(&mut s, statement))
            .await
            .unwrap_or_else(|_| panic!("{statement} outlived its timeout"));
        assert!(matches!(stopped, ExecuteError::Timeout), "{stopped:?}");
    }
}

#[tokio::test]
async fn a_read_goes_where_the_uri_says() {
    let set = mongo_replica_set().await;
    set.run(doc! { "insert": "t", "documents": [{ "_id": 1 }] })
        .await;
    for statement in [
        "db.t.find()",
        "db.t.aggregate([])",
        "db.runCommand({find: 't'})",
        "db.t.countDocuments()",
        "db.t.estimatedDocumentCount()",
        "db.t.distinct('_id')",
        "db.stats()",
    ] {
        // A lost server closes the session, so each statement needs its own.
        let (_dir, mut s) = session(&set, SECONDARY, SessionOptions::default()).await;
        let failed = failure(&mut s, statement).await;
        assert!(
            matches!(failed, ExecuteError::SessionClosed),
            "{statement}: {failed:?}"
        );
    }
    let (_dir, mut s) = session(
        &set,
        "readPreference=primaryPreferred&serverSelectionTimeoutMS=200",
        SessionOptions::default(),
    )
    .await;
    assert_eq!(read(&mut s, "db.t.find()").await.rows.len(), 1);
}

#[tokio::test]
async fn a_catalog_read_goes_where_the_uri_says() {
    let set = mongo_replica_set().await;
    let closed =
        |e: IntrospectError| matches!(e, IntrospectError::Execute(ExecuteError::SessionClosed));
    let t = RelationRef::new("app", "t");

    let (_dir, mut s) = session(&set, SECONDARY, SessionOptions::default()).await;
    assert!(closed(s.schemas().await.unwrap_err()));
    let (_dir, mut s) = session(&set, SECONDARY, SessionOptions::default()).await;
    assert!(closed(s.relations("app").await.unwrap_err()));
    let (_dir, mut s) = session(&set, SECONDARY, SessionOptions::default()).await;
    assert!(closed(s.columns(&t).await.unwrap_err()));
    let (_dir, mut s) = session(&set, SECONDARY, SessionOptions::default()).await;
    assert!(closed(s.relation_objects(&t).await.unwrap_err()));
}

#[tokio::test]
async fn a_write_stays_on_the_primary_whatever_the_uri_says() {
    let set = mongo_replica_set().await;
    let (_dir, mut s) = session(&set, SECONDARY, SessionOptions::default()).await;
    let inserted = one(&mut s, "db.t.insertOne({a: 1})", Limits::default()).await;
    assert_eq!(inserted.outcome.rows_affected, Some(1));
    let raw = "db.runCommand({insert: 't', documents: [{a: 2}]})";
    assert_eq!(read(&mut s, raw).await.rows.len(), 1);
    // `Unknown` may write, so it gets the default as well.
    let unknown = "db.runCommand({findandmodify: 't', query: {a: 2}, update: {$set: {b: 1}}})";
    {
        let mut script = s.script(unknown);
        let p = script.next().await.unwrap().unwrap();
        assert_eq!(p.classification().writes, Writes::Unknown);
    }
    assert_eq!(read(&mut s, unknown).await.rows.len(), 1);
    let changed = set.run(doc! { "count": "t", "query": { "b": 1 } }).await;
    assert_eq!(changed.get_i32("n").unwrap(), 1);
}
