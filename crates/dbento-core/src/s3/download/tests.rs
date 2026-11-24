use super::support::{client, get, head, key, names, stub};
use crate::aws::AwsCancel;
use crate::s3::download::download;
use crate::{DownloadError, DownloadOptions};
use std::time::Duration;

#[tokio::test]
async fn writes_through_a_sibling_partial_then_renames() {
    let s = stub(vec![head(11), get(11, b"hello world", false)]).await;
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("out.bin");
    let report = download(
        &client(s.port),
        &AwsCancel::new(),
        &key(),
        &dest,
        &DownloadOptions::default(),
    )
    .await
    .unwrap();
    assert_eq!(
        (report.bytes, std::fs::read(&dest).unwrap()),
        (11, b"hello world".to_vec())
    );
    assert_eq!(names(dir.path()), ["out.bin"]);
}

#[tokio::test]
async fn the_partial_is_a_hidden_random_sibling_while_bytes_arrive() {
    let s = stub(vec![head(100), get(100, &[7; 10], true)]).await;
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("out.bin");
    let opts = DownloadOptions::default().with_idle_timeout(Duration::from_millis(600));
    let c = client(s.port);
    let cancel = AwsCancel::new();
    let k = key();
    let (result, during) = tokio::join!(download(&c, &cancel, &k, &dest, &opts), async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        names(dir.path())
    });
    assert_eq!(during.len(), 1, "{during:?}");
    let n = &during[0];
    assert!(
        n.starts_with(".out.bin.")
            && n.ends_with(".partial")
            && n.len() == ".out.bin.".len() + 8 + ".partial".len(),
        "{n}"
    );
    assert!(matches!(result, Err(DownloadError::Timeout)), "{result:?}");
    assert!(names(dir.path()).is_empty());
}

#[tokio::test]
async fn an_existing_destination_is_refused_before_any_request_unless_forced() {
    let s = stub(vec![head(3), get(3, b"new", false)]).await;
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("out.bin");
    std::fs::write(&dest, b"old").unwrap();
    let c = client(s.port);
    let e = download(
        &c,
        &AwsCancel::new(),
        &key(),
        &dest,
        &DownloadOptions::default(),
    )
    .await;
    assert!(matches!(e, Err(DownloadError::Exists)), "{e:?}");
    assert_eq!(
        (
            s.seen.load(std::sync::atomic::Ordering::SeqCst),
            std::fs::read(&dest).unwrap()
        ),
        (0, b"old".to_vec())
    );
    download(
        &c,
        &AwsCancel::new(),
        &key(),
        &dest,
        &DownloadOptions::default().with_force(true),
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), b"new");
}

#[cfg(unix)]
#[tokio::test]
async fn a_symlink_destination_is_refused_and_its_target_untouched() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("target");
    std::fs::write(&target, b"keep").unwrap();
    let dest = dir.path().join("out.bin");
    std::os::unix::fs::symlink(&target, &dest).unwrap();
    let s = stub(vec![]).await;
    let e = download(
        &client(s.port),
        &AwsCancel::new(),
        &key(),
        &dest,
        &DownloadOptions::default(),
    )
    .await;
    assert!(matches!(e, Err(DownloadError::Exists)), "{e:?}");
    assert_eq!(std::fs::read(&target).unwrap(), b"keep");
}

#[tokio::test]
async fn a_head_above_max_size_is_refused_before_the_get() {
    let s = stub(vec![head(100)]).await;
    let dir = tempfile::tempdir().unwrap();
    let e = download(
        &client(s.port),
        &AwsCancel::new(),
        &key(),
        &dir.path().join("out.bin"),
        &DownloadOptions::default().with_max_size(10),
    )
    .await;
    assert!(
        matches!(
            e,
            Err(DownloadError::TooLarge {
                size: 100,
                limit: 10
            })
        ),
        "{e:?}"
    );
    assert_eq!(s.seen.load(std::sync::atomic::Ordering::SeqCst), 1);
    assert!(names(dir.path()).is_empty());
}

#[tokio::test]
async fn a_short_body_is_truncated_and_leaves_nothing() {
    let mut short = get(100, &[1; 50], false);
    short.0 = short.0.replace("\r\n\r\n", "\r\nConnection: close\r\n\r\n");
    let s = stub(vec![head(100), short]).await;
    let dir = tempfile::tempdir().unwrap();
    let e = download(
        &client(s.port),
        &AwsCancel::new(),
        &key(),
        &dir.path().join("out.bin"),
        &DownloadOptions::default(),
    )
    .await;
    assert!(matches!(e, Err(DownloadError::Truncated)), "{e:?}");
    assert!(names(dir.path()).is_empty());
}

#[tokio::test]
async fn cancel_mid_body_deletes_the_partial() {
    let s = stub(vec![head(100), get(100, &[7; 10], true)]).await;
    let dir = tempfile::tempdir().unwrap();
    let c = client(s.port);
    let cancel = AwsCancel::new();
    let dest = dir.path().join("out.bin");
    let (k, opts) = (key(), DownloadOptions::default());
    let (result, ()) = tokio::join!(download(&c, &cancel, &k, &dest, &opts), async {
        tokio::time::sleep(Duration::from_millis(300)).await;
        cancel.cancel();
    });
    assert!(
        matches!(result, Err(DownloadError::Cancelled)),
        "{result:?}"
    );
    assert!(names(dir.path()).is_empty());
}

#[tokio::test]
async fn a_missing_directory_is_a_destination_error() {
    let s = stub(vec![head(1), get(1, b"x", false)]).await;
    let dir = tempfile::tempdir().unwrap();
    let e = download(
        &client(s.port),
        &AwsCancel::new(),
        &key(),
        &dir.path().join("no/such/out.bin"),
        &DownloadOptions::default(),
    )
    .await;
    assert!(
        matches!(
            e,
            Err(DownloadError::Destination(std::io::ErrorKind::NotFound))
        ),
        "{e:?}"
    );
}

#[tokio::test]
async fn without_a_content_length_the_head_size_is_the_expected_length() {
    let whole = (
        "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_owned(),
        b"hello world".to_vec(),
        false,
    );
    let short = (
        "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_owned(),
        b"hello".to_vec(),
        false,
    );
    let ok = stub(vec![head(11), whole]).await;
    let dir = tempfile::tempdir().unwrap();
    let dest = dir.path().join("out.bin");
    download(
        &client(ok.port),
        &AwsCancel::new(),
        &key(),
        &dest,
        &DownloadOptions::default(),
    )
    .await
    .unwrap();
    assert_eq!(std::fs::read(&dest).unwrap(), b"hello world");
    let cut = stub(vec![head(11), short]).await;
    let e = download(
        &client(cut.port),
        &AwsCancel::new(),
        &key(),
        &dir.path().join("two.bin"),
        &DownloadOptions::default(),
    )
    .await;
    assert!(matches!(e, Err(DownloadError::Truncated)), "{e:?}");
    assert_eq!(names(dir.path()), ["out.bin"]);
}
