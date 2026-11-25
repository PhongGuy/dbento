//! The object-browser calls, on Moto and on an engine without objects.
use crate::support::sqlite_fixture;
use dbento_core::{ObjectBrowseError, ObjectKey, ObjectList, ObjectRead, SessionOptions};

#[test]
fn a_cursor_compares_and_hashes_by_its_token() {
    use std::collections::HashSet;
    let a = dbento_core::ObjectCursor::new("t1");
    assert_eq!(a, dbento_core::ObjectCursor::new("t1"));
    assert_ne!(a, dbento_core::ObjectCursor::new("t2"));
    let set: HashSet<_> = [a.clone(), dbento_core::ObjectCursor::new("t1")]
        .into_iter()
        .collect();
    assert_eq!(set.len(), 1, "equal tokens hash alike");
}

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
        l.read_object(&ObjectRead::new(k.clone(), 10)).await,
        Err(ObjectBrowseError::Unsupported { .. })
    ));
    let dir = tempfile::tempdir().unwrap();
    let d = l
        .download_object(
            &k,
            &dir.path().join("o"),
            dbento_core::DownloadOptions::default(),
        )
        .await;
    assert!(
        matches!(
            d,
            Err(dbento_core::DownloadError::Remote(
                ObjectBrowseError::Unsupported { .. }
            ))
        ),
        "{d:?}"
    );
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

    #[tokio::test]
    async fn a_three_mib_object_downloads_byte_exact_and_a_missing_one_is_remote() {
        let fx = s3fx("").await;
        let mut s = fx.session().await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("big.bin");
        let r = s
            .download_object(
                &ObjectKey::new(fx.s3.bucket(), "big/three-mib.bin"),
                &dest,
                dbento_core::DownloadOptions::default(),
            )
            .await
            .unwrap();
        assert_eq!(r.bytes, 3 << 20);
        let got = std::fs::read(&dest).unwrap();
        assert!(
            got.iter()
                .enumerate()
                .all(|(i, b)| *b == dbento_testkit::big_byte(i))
        );
        let e = s
            .download_object(
                &ObjectKey::new(fx.s3.bucket(), "nope"),
                &dir.path().join("n"),
                dbento_core::DownloadOptions::default(),
            )
            .await;
        assert!(
            matches!(
                e,
                Err(dbento_core::DownloadError::Remote(
                    ObjectBrowseError::NotFound { .. }
                ))
            ),
            "{e:?}"
        );
    }

    #[tokio::test]
    async fn progress_counts_every_byte_against_the_heads_size() {
        let fx = s3fx("").await;
        let mut s = fx.session().await;
        let dir = tempfile::tempdir().unwrap();
        let key = ObjectKey::new(fx.s3.bucket(), "big/three-mib.bin");
        let progress = dbento_core::DownloadProgress::new();
        assert_eq!((progress.written(), progress.total()), (0, None));
        let opts = dbento_core::DownloadOptions::default().with_progress(progress.clone());
        s.download_object(&key, &dir.path().join("big.bin"), opts)
            .await
            .unwrap();
        assert_eq!(
            (progress.written(), progress.total()),
            (3 << 20, Some(3 << 20))
        );
        let small = dbento_core::DownloadProgress::new();
        let opts = dbento_core::DownloadOptions::default()
            .with_max_size(10)
            .with_progress(small.clone());
        let e = s
            .download_object(&key, &dir.path().join("cap.bin"), opts)
            .await;
        assert!(
            matches!(e, Err(dbento_core::DownloadError::TooLarge { .. })),
            "{e:?}"
        );
        assert_eq!(
            (small.written(), small.total()),
            (0, Some(3 << 20)),
            "the size is known before the refusal"
        );
    }
}
