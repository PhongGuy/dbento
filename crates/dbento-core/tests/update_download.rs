//! The artifact download: caps, hash, staging and cleanup.
#![cfg(feature = "update")]

use crate::update_support::{TestKey, leftovers, offer, offer_as, offer_within, serve_artifact};
use dbento_core::update::{DownloadError, download};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use wiremock::{MockServer, ResponseTemplate};

const BODY: &[u8] = b"new binary";
const SUFFIX: &str = if cfg!(windows) { ".exe" } else { "" };

async fn setup() -> (MockServer, TestKey, tempfile::TempDir) {
    (
        MockServer::start().await,
        TestKey::generate(),
        tempfile::tempdir().unwrap(),
    )
}

type Seen = Arc<Mutex<Vec<(u64, u64)>>>;

fn recorder() -> (Seen, impl FnMut(u64, u64) + Send + 'static) {
    let seen: Seen = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&seen);
    (seen, move |got, total| {
        sink.lock().unwrap().push((got, total))
    })
}

fn file_name(path: &Path) -> String {
    path.file_name().unwrap().to_string_lossy().into_owned()
}

// A staged file sits in `dir` under the version's prefix, a per-download stem, then `suffix`.
fn assert_staged_as(path: &Path, dir: &Path, suffix: &str) {
    assert_eq!(path.parent(), Some(dir), "{}", path.display());
    let name = file_name(path);
    let stem = name
        .strip_prefix("dbento-update-99.0.0-")
        .and_then(|rest| rest.strip_suffix(suffix));
    assert!(
        stem.is_some_and(|s| !s.is_empty() && !s.contains('.')),
        "{name}"
    );
}

// Windows locks are mandatory, so the staged bytes are read once the hold is gone.
#[tokio::test]
async fn a_verified_download_is_staged() {
    let (s, k, dir) = setup().await;
    serve_artifact(&s, ResponseTemplate::new(200).set_body_bytes(BODY)).await;
    let a = offer(&s, &k, BODY).await;
    let (seen, progress) = recorder();
    let staged = download(&a, dir.path(), progress).await.unwrap();
    assert_eq!(staged.version(), "99.0.0");
    assert_staged_as(staged.path(), dir.path(), SUFFIX);
    assert_eq!(seen.lock().unwrap().last(), Some(&(10, 10)));
    assert_eq!(leftovers(dir.path()), [file_name(staged.path())]);
    // The staged file stays private to the user until it is installed.
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let meta = std::fs::symlink_metadata(staged.path()).unwrap();
        assert!(meta.file_type().is_file(), "{meta:?}");
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    }
    let path = staged.path().to_path_buf();
    drop(staged);
    assert_eq!(std::fs::read(&path).unwrap(), BODY);
}

#[tokio::test]
async fn each_format_keeps_the_suffix_its_installer_needs() {
    for (format, suffix) in [
        ("appimage", ".AppImage"),
        ("app", ".app.tar.gz"),
        ("nsis", ".exe"),
    ] {
        let s = MockServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        let a = offer_as(&s, format, BODY).await;
        let staged = download(&a, dir.path(), |_, _| {}).await.unwrap();
        assert_staged_as(staged.path(), dir.path(), suffix);
    }
}

#[tokio::test]
async fn an_encoded_body_is_kept_as_sent() {
    let (s, k, dir) = setup().await;
    let gz = [0x1f, 0x8b, 8, 0, 1, 2, 3];
    let encoded = ResponseTemplate::new(200)
        .insert_header("content-encoding", "gzip")
        .set_body_bytes(gz);
    serve_artifact(&s, encoded).await;
    let staged = download(&offer(&s, &k, &gz).await, dir.path(), |_, _| {})
        .await
        .unwrap();
    let path = staged.path().to_path_buf();
    drop(staged);
    assert_eq!(std::fs::read(&path).unwrap(), gz);
}

#[tokio::test]
async fn a_longer_body_stops_and_leaves_nothing() {
    let (s, k, dir) = setup().await;
    serve_artifact(
        &s,
        ResponseTemplate::new(200).set_body_bytes(vec![b'x'; 1024 * 1024]),
    )
    .await;
    let (seen, progress) = recorder();
    let got = download(&offer(&s, &k, BODY).await, dir.path(), progress).await;
    assert!(
        matches!(got, Err(DownloadError::SizeMismatch { expected: 10, .. })),
        "{got:?}"
    );
    let seen = seen.lock().unwrap().clone();
    assert!(
        seen.iter().all(|&(n, _)| n <= 10),
        "wrote past the cap: {seen:?}"
    );
    assert!(leftovers(dir.path()).is_empty());
}

#[tokio::test]
async fn a_short_body_is_refused() {
    let (s, k, dir) = setup().await;
    serve_artifact(
        &s,
        ResponseTemplate::new(200).set_body_bytes(b"new ".to_vec()),
    )
    .await;
    let got = download(&offer(&s, &k, BODY).await, dir.path(), |_, _| {}).await;
    assert_eq!(
        got.unwrap_err(),
        DownloadError::SizeMismatch {
            expected: 10,
            got: 4
        }
    );
    assert!(leftovers(dir.path()).is_empty());
}

#[tokio::test]
async fn altered_bytes_fail_the_hash() {
    let (s, k, dir) = setup().await;
    serve_artifact(
        &s,
        ResponseTemplate::new(200).set_body_bytes(b"bad binary".to_vec()),
    )
    .await;
    let got = download(&offer(&s, &k, BODY).await, dir.path(), |_, _| {}).await;
    assert_eq!(got.unwrap_err(), DownloadError::HashMismatch);
    assert!(leftovers(dir.path()).is_empty());
}

#[tokio::test]
async fn a_missing_artifact_and_a_server_error() {
    let (s, k, dir) = setup().await;
    serve_artifact(&s, ResponseTemplate::new(404)).await;
    let got = download(&offer(&s, &k, BODY).await, dir.path(), |_, _| {}).await;
    assert_eq!(got.unwrap_err(), DownloadError::ArtifactMissing);

    let (s, k, dir) = setup().await;
    serve_artifact(&s, ResponseTemplate::new(503)).await;
    let got = download(&offer(&s, &k, BODY).await, dir.path(), |_, _| {}).await;
    assert_eq!(got.unwrap_err(), DownloadError::Http(503));
    assert!(leftovers(dir.path()).is_empty());
}

// A crash's `.part` is never resumed or touched by a download; the app's sweep removes it.
#[tokio::test]
async fn a_stale_part_is_left_alone_and_never_resumed() {
    let (s, k, dir) = setup().await;
    let stale = dir
        .path()
        .join(format!("dbento-update-99.0.0{SUFFIX}.part"));
    std::fs::write(&stale, b"junk from a crash").unwrap();
    serve_artifact(&s, ResponseTemplate::new(200).set_body_bytes(BODY)).await;
    let staged = download(&offer(&s, &k, BODY).await, dir.path(), |_, _| {})
        .await
        .unwrap();
    let path = staged.path().to_path_buf();
    drop(staged);
    assert_eq!(std::fs::read(&path).unwrap(), BODY);
    assert_eq!(std::fs::read(&stale).unwrap(), b"junk from a crash");
    assert_eq!(
        leftovers(dir.path()).len(),
        2,
        "{:?}",
        leftovers(dir.path())
    );
}

#[tokio::test]
async fn an_unwritable_staging_dir_is_refused_before_downloading() {
    let (s, k, dir) = setup().await;
    serve_artifact(&s, ResponseTemplate::new(200).set_body_bytes(BODY)).await;
    let a = offer(&s, &k, BODY).await;
    let missing = dir.path().join("missing");
    assert_eq!(
        download(&a, &missing, |_, _| {}).await.unwrap_err(),
        DownloadError::NotWritable(missing)
    );
    let requests = s.received_requests().await.unwrap();
    assert_eq!(
        requests
            .iter()
            .filter(|r| r.url.path() == "/artifact")
            .count(),
        0
    );
}

#[tokio::test]
async fn a_slow_artifact_times_out_and_leaves_nothing() {
    let (s, k, dir) = setup().await;
    let slow = ResponseTemplate::new(200)
        .set_body_bytes(BODY)
        .set_delay(Duration::from_secs(5));
    serve_artifact(&s, slow).await;
    let a = offer_within(&s, &k, BODY, Duration::from_millis(200)).await;
    assert_eq!(
        download(&a, dir.path(), |_, _| {}).await.unwrap_err(),
        DownloadError::Timeout
    );
    assert!(leftovers(dir.path()).is_empty());
}

// The staged file stays locked while its `Staged` lives, so another copy can tell it is in use.
#[tokio::test]
async fn a_staged_download_stays_locked_until_dropped() {
    let (s, k, dir) = setup().await;
    serve_artifact(&s, ResponseTemplate::new(200).set_body_bytes(BODY)).await;
    let staged = download(&offer(&s, &k, BODY).await, dir.path(), |_, _| {})
        .await
        .unwrap();
    let probe = std::fs::File::open(staged.path()).unwrap();
    assert!(
        matches!(probe.try_lock(), Err(std::fs::TryLockError::WouldBlock)),
        "the staged file is not held"
    );
    let path = staged.path().to_path_buf();
    drop(staged);
    assert!(probe.try_lock().is_ok(), "the hold outlived its Staged");
    drop(probe);
    assert_eq!(std::fs::read(&path).unwrap(), BODY);
}

// Two copies downloading the same release, one after the other or at once, never collide.
#[tokio::test]
async fn downloads_of_one_release_into_one_dir_get_their_own_files() {
    let (s, k, dir) = setup().await;
    serve_artifact(&s, ResponseTemplate::new(200).set_body_bytes(BODY)).await;
    let a = offer(&s, &k, BODY).await;
    let first = download(&a, dir.path(), |_, _| {}).await.unwrap();
    let second = download(&a, dir.path(), |_, _| {}).await.unwrap();
    let (third, fourth) = tokio::join!(
        download(&a, dir.path(), |_, _| {}),
        download(&a, dir.path(), |_, _| {})
    );
    let all = [first, second, third.unwrap(), fourth.unwrap()];
    let paths: Vec<_> = all.iter().map(|st| st.path().to_path_buf()).collect();
    let mut distinct = paths.clone();
    distinct.sort();
    distinct.dedup();
    assert_eq!(distinct.len(), 4, "{paths:?}");
    drop(all);
    for p in &paths {
        assert_eq!(std::fs::read(p).unwrap(), BODY, "{}", p.display());
    }
    assert_eq!(
        leftovers(dir.path()).len(),
        4,
        "{:?}",
        leftovers(dir.path())
    );
}
