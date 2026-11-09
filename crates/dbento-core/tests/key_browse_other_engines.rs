//! Key browsing on an engine that is not Redis, in every build.

use crate::support::sqlite_fixture;
use dbento_core::{KeyBrowseError, KeyRead, KeyScan, SessionOptions};

#[tokio::test]
async fn sqlite_has_no_keys_to_scan_or_read() {
    let lite = sqlite_fixture("CREATE TABLE t (a)", "");
    let mut l = lite.session(SessionOptions::default()).await;
    let e = l.scan_keys(&KeyScan::new(0)).await.unwrap_err();
    assert!(matches!(e, KeyBrowseError::Unsupported { .. }), "{e:?}");
    let e = l
        .read_key(&KeyRead::new(0, b"k".to_vec()))
        .await
        .unwrap_err();
    assert!(matches!(e, KeyBrowseError::Unsupported { .. }), "{e:?}");
}

#[tokio::test]
async fn sqlite_has_no_command_docs() {
    let lite = sqlite_fixture("CREATE TABLE t (a)", "");
    let mut l = lite.session(SessionOptions::default()).await;
    assert!(matches!(
        l.command_docs().await,
        Err(KeyBrowseError::Unsupported { .. })
    ));
}
