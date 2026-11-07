//! The key browser's scan: every key once, filters, and the editor's DB untouched.
#![cfg(feature = "redis")]
mod support;

use std::collections::BTreeSet;
use std::time::Duration;

use dbento_core::{KeyBrowseError, KeyEntry, KeyKind, KeyScan, KeyTtl, Session, SessionOptions};
use dbento_testkit::{
    BINARY_KEY, MANY_KEYS,
    RdUser::{Reader, Writer},
    SEED_KEYS,
};
use support::{client_field, forced, rd, read, sqlite_fixture};

async fn every_key(s: &mut Session, mut req: KeyScan) -> Vec<KeyEntry> {
    let mut out = Vec::new();
    loop {
        let page = s.scan_keys(&req).await.unwrap();
        out.extend(page.keys);
        match page.next {
            Some(next) => req.cursor = next,
            None => return out,
        }
    }
}

#[tokio::test]
async fn a_scan_finds_every_seeded_key_exactly_once() {
    let fx = rd("").await;
    let mut s = fx.session(Reader, forced()).await;
    let mut req = KeyScan::new(fx.db.index());
    req.want = 300;
    let keys = every_key(&mut s, req).await;
    let names: BTreeSet<Vec<u8>> = keys.iter().map(|k| k.name.clone()).collect();
    assert_eq!(
        (keys.len(), names.len()),
        (SEED_KEYS as usize, SEED_KEYS as usize)
    );
    assert!(names.contains(BINARY_KEY));
    let entry = |n: &[u8]| keys.iter().find(|k| k.name == n).unwrap().clone();
    assert_eq!(entry(b"hash").kind, KeyKind::Hash);
    assert_eq!(entry(b"zset").kind, KeyKind::SortedSet);
    assert_eq!(entry(b"stream").kind, KeyKind::Stream);
    assert_eq!(entry(b"str").ttl, KeyTtl::Persistent);
    assert!(matches!(entry(b"ttl").ttl, KeyTtl::Expires(d) if d <= Duration::from_secs(3_600)));
}

#[tokio::test]
async fn a_pattern_and_a_kind_narrow_the_scan() {
    let fx = rd("").await;
    let mut s = fx.session(Reader, SessionOptions::default()).await;
    let mut many = KeyScan::new(fx.db.index());
    many.pattern = Some("many:*".to_owned());
    many.want = 100;
    assert_eq!(every_key(&mut s, many).await.len(), MANY_KEYS);
    let mut hashes = KeyScan::new(fx.db.index());
    hashes.kind = Some(KeyKind::Hash);
    let names: BTreeSet<_> = every_key(&mut s, hashes)
        .await
        .into_iter()
        .map(|k| k.name)
        .collect();
    assert_eq!(
        names,
        BTreeSet::from([b"big:hash".to_vec(), b"hash".to_vec()])
    );
}

#[tokio::test]
async fn a_scan_leaves_the_editor_db_where_it_was() {
    let fx = rd("").await;
    let mut s = fx.session(Writer, SessionOptions::default()).await;
    for line in [
        "MULTI".to_owned(),
        format!("SELECT {}", fx.other_db()),
        "EXEC".to_owned(),
    ] {
        read(&mut s, &line).await;
    }
    s.scan_keys(&KeyScan::new(fx.db.index())).await.unwrap();
    assert_eq!(client_field(&mut s, "db").await, fx.other_db().to_string());
}

#[tokio::test]
async fn a_scan_is_refused_inside_multi_and_on_other_engines() {
    let fx = rd("").await;
    let mut s = fx.session(Writer, SessionOptions::default()).await;
    read(&mut s, "MULTI").await;
    let e = s.scan_keys(&KeyScan::new(fx.db.index())).await.unwrap_err();
    assert!(matches!(e, KeyBrowseError::Unsupported { .. }), "{e:?}");
    read(&mut s, "DISCARD").await;
    let lite = sqlite_fixture("CREATE TABLE t (a)", "");
    let mut l = lite.session(SessionOptions::default()).await;
    let e = l.scan_keys(&KeyScan::new(0)).await.unwrap_err();
    assert!(matches!(e, KeyBrowseError::Unsupported { .. }), "{e:?}");
}

#[tokio::test]
async fn a_db_out_of_range_is_a_database_error() {
    let fx = rd("").await;
    let mut s = fx.session(Reader, SessionOptions::default()).await;
    let e = s.scan_keys(&KeyScan::new(999)).await.unwrap_err();
    assert!(
        matches!(e, KeyBrowseError::Database { ref code, .. } if code == "ERR"),
        "{e:?}"
    );
}
