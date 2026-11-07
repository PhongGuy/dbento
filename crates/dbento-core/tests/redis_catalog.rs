//! DB indexes with key counts, the unsupported relation calls and COMMAND DOCS.
#![cfg(feature = "redis")]
mod support;

use dbento_core::{IntrospectError, KeyBrowseError, RelationRef, SessionOptions};
use dbento_testkit::{
    RdUser::{Admin, Reader, Writer},
    SEED_KEYS,
};
use support::{client_field, rd, read, sqlite_fixture};

#[tokio::test]
async fn schemas_list_every_db_with_its_key_count() {
    let fx = rd("").await;
    for user in [Reader, Admin] {
        let mut s = fx.session(user, SessionOptions::default()).await;
        let schemas = s.schemas().await.unwrap();
        if fx.db.exclusive() {
            assert_eq!(schemas.len(), 16, "{user:?}");
        }
        let mine = &schemas[fx.db.index() as usize];
        assert_eq!(mine.name(), format!("db{}", fx.db.index()));
        assert_eq!(mine.key_count(), Some(SEED_KEYS), "{user:?}");
        assert_eq!(client_field(&mut s, "db").await, fx.db.index().to_string());
        s.close().await.unwrap();
    }
}

#[tokio::test]
async fn relation_calls_point_to_scan_and_multi_refuses_the_list() {
    let fx = rd("").await;
    let mut s = fx.session(Writer, SessionOptions::default()).await;
    match s.relations("db0").await {
        Err(IntrospectError::Unsupported { what }) => assert!(what.contains("SCAN"), "{what}"),
        other => panic!("{other:?}"),
    }
    let rel = RelationRef::new("db0", "k");
    assert!(matches!(
        s.columns(&rel).await,
        Err(IntrospectError::Unsupported { .. })
    ));
    assert!(matches!(
        s.relation_objects(&rel).await,
        Err(IntrospectError::Unsupported { .. })
    ));
    read(&mut s, "MULTI").await;
    assert!(matches!(
        s.schemas().await,
        Err(IntrospectError::Unsupported { .. })
    ));
    read(&mut s, "DISCARD").await;
}

#[tokio::test]
async fn the_reader_reads_command_docs_and_other_engines_have_none() {
    let fx = rd("").await;
    let mut s = fx.session(Reader, SessionOptions::default()).await;
    let docs = s.command_docs().await.unwrap();
    assert!(docs.iter().any(|d| d.name == "hgetall"), "{}", docs.len());
    let lite = sqlite_fixture("CREATE TABLE t (a)", "");
    let mut l = lite.session(SessionOptions::default()).await;
    assert!(matches!(
        l.command_docs().await,
        Err(KeyBrowseError::Unsupported { .. })
    ));
}
