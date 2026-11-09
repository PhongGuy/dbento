//! A MongoDB session's dispatch: what the engine lacks answers at once.
#![cfg(feature = "mongodb")]

use std::time::Duration;

use crate::support::mg;
use dbento_core::{
    DefinitionError, Engine, IntrospectError, ObjectRef, RelationRef, SessionOptions,
};
use dbento_testkit::MgUser::Reader;

#[tokio::test]
async fn what_the_engine_lacks_answers_as_unsupported_or_empty() {
    let fx = mg("").await;
    let mut s = fx.session(Reader, SessionOptions::default()).await;
    assert_eq!(s.engine(), Engine::Mongodb);
    assert!(!s.settings_changed());
    let rel = RelationRef::new(fx.db.database(), "many");
    let target = s.edit_target(&rel).await;
    assert!(matches!(
        target,
        Err(IntrospectError::Unsupported {
            what: "edit targets"
        })
    ));
    assert!(s.foreign_keys(&rel).await.unwrap().is_empty());
    let objects = s.schema_objects(fx.db.database()).await.unwrap();
    assert!(objects.types().is_empty() && objects.routines().is_empty());
    let definition = s.definition(&ObjectRef::relation(rel.clone())).await;
    assert!(matches!(
        definition,
        Err(DefinitionError::Unsupported { .. })
    ));
    assert!(s.source_columns(&rel, &[]).await.unwrap().is_empty());
    assert!(
        tokio::time::timeout(Duration::from_millis(50), s.closed())
            .await
            .is_err()
    );
    s.close().await.unwrap();
}
