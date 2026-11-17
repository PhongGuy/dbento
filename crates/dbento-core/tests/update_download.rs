//! The artifact download: caps, hash, staging and cleanup.
#![cfg(feature = "update")]

use crate::update_support::{TestKey, leftovers, offer, offer_as, offer_within, serve_artifact};
use dbento_core::update::{DownloadError, download};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use wiremock::{MockServer, ResponseTemplate};

const BODY: &[u8] = b"new binary";
const STAGED: &str = if cfg!(windows) {
    "dbento-update-99.0.0.exe"
} else {
    "dbento-update-99.0.0"
};

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

#[tokio::test]
async fn a_verified_download_is_staged() {
    let (s, k, dir) = setup().await;
    serve_artifact(&s, ResponseTemplate::new(200).set_body_bytes(BODY)).await;
    let a = offer(&s, &k, BODY).await;
    let (seen, progress) = recorder();
    let staged = download(&a, dir.path(), progress).await.unwrap();
    assert_eq!(std::fs::read(staged.path()).unwrap(), BODY);
    assert_eq!(staged.version(), "99.0.0");
    assert_eq!(staged.path(), dir.path().join(STAGED));
    assert_eq!(seen.lock().unwrap().last(), Some(&(10, 10)));
    assert_eq!(leftovers(dir.path()), [STAGED]);
}

#[tokio::test]
async fn each_format_keeps_the_suffix_its_installer_needs() {
    for (format, name) in [
        ("appimage", "dbento-update-99.0.0.AppImage"),
        ("app", "dbento-update-99.0.0.app.tar.gz"),
        ("nsis", "dbento-update-99.0.0.exe"),
    ] {
        let s = MockServer::start().await;
        let dir = tempfile::tempdir().unwrap();
        let a = offer_as(&s, format, BODY).await;
        let staged = download(&a, dir.path(), |_, _| {}).await.unwrap();
        assert_eq!(staged.path(), dir.path().join(name), "{format}");
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
    assert_eq!(std::fs::read(staged.path()).unwrap(), gz);
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

#[tokio::test]
async fn a_stale_part_is_replaced() {
    let (s, k, dir) = setup().await;
    std::fs::write(
        dir.path().join(format!("{STAGED}.part")),
        b"junk from a crash",
    )
    .unwrap();
    serve_artifact(&s, ResponseTemplate::new(200).set_body_bytes(BODY)).await;
    download(&offer(&s, &k, BODY).await, dir.path(), |_, _| {})
        .await
        .unwrap();
    assert_eq!(leftovers(dir.path()), [STAGED]);
}

// A link planted at the staging name must never redirect the download's bytes elsewhere.
#[cfg(unix)]
#[tokio::test]
async fn a_planted_symlink_at_the_part_is_never_followed() {
    use std::os::unix::fs::PermissionsExt as _;
    let (s, k, dir) = setup().await;
    let outside = tempfile::tempdir().unwrap();
    let sentinel = outside.path().join("sentinel");
    std::fs::write(&sentinel, b"keep me").unwrap();
    std::os::unix::fs::symlink(&sentinel, dir.path().join(format!("{STAGED}.part"))).unwrap();
    serve_artifact(&s, ResponseTemplate::new(200).set_body_bytes(BODY)).await;
    let got = download(&offer(&s, &k, BODY).await, dir.path(), |_, _| {}).await;
    assert_eq!(std::fs::read(&sentinel).unwrap(), b"keep me");
    if let Ok(staged) = got {
        let meta = std::fs::symlink_metadata(staged.path()).unwrap();
        assert!(meta.file_type().is_file(), "{meta:?}");
        // The staged file stays private to the user until it is installed.
        assert_eq!(meta.permissions().mode() & 0o777, 0o600);
    }
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
