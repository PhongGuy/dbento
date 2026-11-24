//! The object-browser calls, on Moto and on an engine without objects.
use crate::support::sqlite_fixture;
use dbento_core::{ObjectBrowseError, ObjectKey, ObjectList, ObjectRead, SessionOptions};

#[tokio::test]
async fn sqlite_has_no_objects_to_browse() {
    let lite = sqlite_fixture("CREATE TABLE t (a)", "");
    let mut l = lite.session(SessionOptions::default()).await;
    let k = ObjectKey::new("b", "k");
    assert!(matches!(
        l.list_objects(&ObjectList::new("b", "")).await,
        Err(ObjectBrowseError::Unsupported { .. })
    ));
    assert!(matches!(
        l.stat_object(&k).await,
        Err(ObjectBrowseError::Unsupported { .. })
    ));
    assert!(matches!(
        l.read_object(&ObjectRead::new(k, 10)).await,
        Err(ObjectBrowseError::Unsupported { .. })
    ));
}

#[cfg(feature = "s3")]
mod moto {
    use super::*;
    use crate::s3_session::s3fx;

    #[tokio::test]
    async fn pages_follow_the_cursor_and_never_exceed_a_thousand() {
        let fx = s3fx("").await;
        fx.s3.seed_paged().await;
        let mut s = fx.session().await;
        let mut req = ObjectList::new(fx.s3.bucket(), "many/").want(5_000);
        let mut sizes = Vec::new();
        loop {
            let page = s.list_objects(&req).await.unwrap();
            sizes.push(page.objects.len());
            match page.next {
                Some(c) => req = ObjectList::new(fx.s3.bucket(), "many/").after(c),
                None => break,
            }
        }
        assert_eq!(sizes, [1000, 1000, 500]);
    }

    #[tokio::test]
    async fn the_root_shows_prefixes_and_deep_prefixes_open_one_level() {
        let fx = s3fx("").await;
        let mut s = fx.session().await;
        let root = s
            .list_objects(&ObjectList::new(fx.s3.bucket(), ""))
            .await
            .unwrap();
        assert!(root.objects.is_empty() && root.prefixes.contains(&"deep/".to_owned()));
        let deep = s
            .list_objects(&ObjectList::new(fx.s3.bucket(), "deep/a/"))
            .await
            .unwrap();
        assert_eq!(deep.prefixes, ["deep/a/b/"]);
        assert_eq!(
            deep.objects
                .iter()
                .map(|o| o.key.as_str())
                .collect::<Vec<_>>(),
            ["deep/a/top.txt"]
        );
        let all = s
            .list_objects(&ObjectList::new(fx.s3.bucket(), "deep/").recursive())
            .await
            .unwrap();
        assert_eq!(all.objects.len(), 2);
    }

    #[tokio::test]
    async fn stat_and_read_cap_at_a_mebibyte_and_an_empty_object_reads_empty() {
        let fx = s3fx("").await;
        let mut s = fx.session().await;
        let big = ObjectKey::new(fx.s3.bucket(), "big/three-mib.bin");
        assert_eq!(s.stat_object(&big).await.unwrap().size, 3 << 20);
        let chunk = s
            .read_object(&ObjectRead::new(big, u32::MAX))
            .await
            .unwrap();
        assert_eq!(
            (chunk.bytes.len(), chunk.total_size, chunk.truncated),
            (1 << 20, 3 << 20, true)
        );
        let zero = s
            .read_object(&ObjectRead::new(
                ObjectKey::new(fx.s3.bucket(), "empty/zero"),
                10,
            ))
            .await
            .unwrap();
        assert_eq!(
            (zero.bytes.len(), zero.total_size, zero.truncated),
            (0, 0, false)
        );
        let missing = s.stat_object(&ObjectKey::new(fx.s3.bucket(), "nope")).await;
        assert!(
            matches!(missing, Err(ObjectBrowseError::NotFound { .. })),
            "{missing:?}"
        );
    }
}
