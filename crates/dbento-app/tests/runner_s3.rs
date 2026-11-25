//! S3 through the app's runners on Moto: the catalog lists levels and pages, and a tab's
//! runner reads an object.
#![cfg(feature = "s3")]
use std::collections::HashSet;
use std::time::Duration;

use crate::support::{here, s3_spec};
use dbento_app::catalog::{CatalogData, CatalogQuery, CatalogRunner, spawn_catalog};
use dbento_app::objects::{OBJECT_PAGE, ObjectBatch, ObjectEntry};
use dbento_testkit::{HOSTILE_KEYS, PAGED, S3Moto, s3_moto};

fn objects(s3: &S3Moto, prefix: &str, after: Option<dbento_core::ObjectCursor>) -> CatalogQuery {
    CatalogQuery::Objects {
        bucket: s3.bucket().into(),
        prefix: prefix.into(),
        after,
    }
}
async fn page(r: &CatalogRunner, query: CatalogQuery) -> Result<CatalogData, String> {
    r.load(0, query);
    tokio::time::timeout(Duration::from_secs(30), r.replies().recv())
        .await
        .unwrap()
        .unwrap()
        .result
}

#[tokio::test(flavor = "multi_thread")]
async fn the_catalog_pages_a_prefix_and_every_key_arrives_once() {
    let s3 = s3_moto().await;
    s3.seed_paged().await;
    let (_dir, spec) = s3_spec(&s3, "");
    let r = spawn_catalog(&here(), spec);
    let (mut seen, mut after, mut pages) = (HashSet::new(), None, 0);
    loop {
        let data = page(&r, objects(&s3, "many/", after.clone()))
            .await
            .unwrap();
        let CatalogData::Objects(ObjectBatch { entries, next }) = data else {
            panic!("{data:?}")
        };
        assert!(entries.len() <= OBJECT_PAGE as usize, "{}", entries.len());
        for e in entries {
            let ObjectEntry::Object(o) = e else {
                panic!("no prefixes under many/: {e:?}")
            };
            assert!(seen.insert(o.key.clone()), "{} twice", o.key);
        }
        pages += 1;
        match next {
            Some(n) => after = Some(n),
            None => break,
        }
    }
    assert_eq!(seen.len(), PAGED);
    assert!(pages >= PAGED / OBJECT_PAGE as usize, "{pages} pages");
}

#[tokio::test(flavor = "multi_thread")]
async fn the_root_lists_the_seed_prefixes_and_hostile_keys_arrive_raw() {
    let s3 = s3_moto().await;
    let (_dir, spec) = s3_spec(&s3, "");
    let r = spawn_catalog(&here(), spec);
    let Ok(CatalogData::Objects(top)) = page(&r, objects(&s3, "", None)).await else {
        panic!("root")
    };
    let prefixes: Vec<&str> = top
        .entries
        .iter()
        .filter_map(|e| match e {
            ObjectEntry::Prefix(p) => Some(p.as_str()),
            ObjectEntry::Object(_) => None,
        })
        .collect();
    assert_eq!(
        prefixes,
        [
            "big/", "bin/", "deep/", "empty/", "hostile/", "json/", "text/", "unicode/"
        ]
    );
    assert_eq!(top.next, None);
    let Ok(CatalogData::Objects(h)) = page(&r, objects(&s3, "hostile/", None)).await else {
        panic!("hostile")
    };
    let keys: HashSet<&str> = h
        .entries
        .iter()
        .filter_map(|e| match e {
            ObjectEntry::Object(o) => Some(o.key.as_str()),
            ObjectEntry::Prefix(_) => None,
        })
        .collect();
    for k in HOSTILE_KEYS.iter().filter(|k| !k.contains("/../")) {
        assert!(keys.contains(k), "{k:?} in {keys:?}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a_missing_bucket_reads_as_not_found_with_its_prefix_escaped() {
    let s3 = s3_moto().await;
    let (_dir, spec) = s3_spec(&s3, "");
    let r = spawn_catalog(&here(), spec);
    let q = CatalogQuery::Objects {
        bucket: "dbento-missing-bucket".into(),
        prefix: "a\nb/".into(),
        after: None,
    };
    let e = page(&r, q).await.unwrap_err();
    assert_eq!(e, "not found: s3://dbento-missing-bucket/a\\u{a}b/");
}
