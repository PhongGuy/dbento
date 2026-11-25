//! Fetching the offer's release notes over a loopback server: checks, caps and failures that never touch the offer.
#![cfg(feature = "update")]

use crate::update_support::{TestKey, config, manifest, manifest_with_notes, mount, publish};
use dbento_core::update::{Available, NotesError, Timeouts, check, release_notes};
use std::time::Duration;
use wiremock::{MockServer, ResponseTemplate};

const ART: &str = "https://example.com/dbento";
const NOTES: &[u8] =
    br#"{"schema":1,"releases":[{"version":"99.0.0","previous":"1.0.0","groups":[]}]}"#;

/// Publishes 99.0.0 whose manifest lists `listed` at `<server>/notes.json`, serves `served` there, and returns the offer.
async fn offer(
    s: &MockServer,
    listed: &[u8],
    served: ResponseTemplate,
    manifest_timeout: Duration,
) -> Available {
    let k = TestKey::generate();
    let notes_url = format!("{}/notes.json", s.uri());
    publish(
        s,
        &k,
        &manifest_with_notes("99.0.0", ART, b"new", &notes_url, listed),
    )
    .await;
    mount(s, "/notes.json", served).await;
    let mut t = Timeouts::default();
    t.manifest = manifest_timeout;
    let cfg = config(&s.uri(), &k, "1.0.0").with_timeouts(t);
    check(&cfg)
        .await
        .unwrap()
        .expect("the update is still offered")
}

async fn notes_requests(s: &MockServer) -> usize {
    let requests = s.received_requests().await.unwrap();
    requests
        .iter()
        .filter(|r| r.url.path() == "/notes.json")
        .count()
}

fn ok(body: &[u8]) -> ResponseTemplate {
    ResponseTemplate::new(200).set_body_bytes(body.to_vec())
}

#[tokio::test]
async fn listed_notes_are_fetched_checked_and_parsed() {
    let s = MockServer::start().await;
    let a = offer(&s, NOTES, ok(NOTES), Duration::from_secs(30)).await;
    assert_eq!(notes_requests(&s).await, 0, "the check fetched the notes");
    let n = release_notes(&a).await.unwrap();
    assert_eq!(n.releases()[0].version(), "99.0.0");
    let requests = s.received_requests().await.unwrap();
    let r = requests
        .iter()
        .find(|r| r.url.path() == "/notes.json")
        .unwrap();
    let mut names: Vec<&str> = r.headers.keys().map(|n| n.as_str()).collect();
    names.sort_unstable();
    assert_eq!(names, ["accept", "accept-encoding", "host", "user-agent"]);
}

#[tokio::test]
async fn a_manifest_without_notes_gives_no_notes() {
    let s = MockServer::start().await;
    let k = TestKey::generate();
    publish(&s, &k, &manifest("99.0.0", ART, b"new")).await;
    let a = check(&config(&s.uri(), &k, "1.0.0"))
        .await
        .unwrap()
        .expect("offered");
    assert_eq!(
        release_notes(&a).await.unwrap_err(),
        NotesError::NotPublished
    );
    assert_eq!(notes_requests(&s).await, 0);
}

#[tokio::test]
async fn missing_notes_are_an_http_error() {
    let s = MockServer::start().await;
    let a = offer(
        &s,
        NOTES,
        ResponseTemplate::new(404),
        Duration::from_secs(30),
    )
    .await;
    assert_eq!(release_notes(&a).await.unwrap_err(), NotesError::Http(404));
}

#[tokio::test]
async fn slow_notes_time_out() {
    let s = MockServer::start().await;
    let slow = ok(NOTES).set_delay(Duration::from_secs(5));
    let a = offer(&s, NOTES, slow, Duration::from_millis(300)).await;
    assert_eq!(release_notes(&a).await.unwrap_err(), NotesError::Timeout);
}

#[tokio::test]
async fn oversize_notes_are_refused() {
    let s = MockServer::start().await;
    let big = vec![b' '; 64 * 1024 + 1];
    let a = offer(&s, &big, ok(&big), Duration::from_secs(30)).await;
    assert_eq!(release_notes(&a).await.unwrap_err(), NotesError::TooLarge);
    assert_eq!(
        notes_requests(&s).await,
        0,
        "an oversize listing was fetched"
    );

    let s = MockServer::start().await;
    let longer = [NOTES, b"    "].concat();
    let a = offer(&s, NOTES, ok(&longer), Duration::from_secs(30)).await;
    assert_eq!(
        release_notes(&a).await.unwrap_err(),
        NotesError::SizeMismatch
    );
}

#[tokio::test]
async fn a_bad_hash_is_refused_before_parsing() {
    let s = MockServer::start().await;
    let other = br#"{"schema":2,"releases":[{"version":"99.0.0","previous":"1.0.0","groups":[]}]}"#;
    assert_eq!(other.len(), NOTES.len(), "the swap must keep the size");
    let a = offer(&s, NOTES, ok(other), Duration::from_secs(30)).await;
    assert_eq!(
        release_notes(&a).await.unwrap_err(),
        NotesError::HashMismatch
    );
}

#[tokio::test]
async fn a_malformed_notes_asset_still_offers_the_update() {
    let s = MockServer::start().await;
    let k = TestKey::generate();
    let m = manifest("99.0.0", ART, b"new").replacen(
        '{',
        r#"{"notes_asset":{"url":"ftp://x","size":1,"sha256":"AB"},"#,
        1,
    );
    publish(&s, &k, &m).await;
    let a = check(&config(&s.uri(), &k, "1.0.0"))
        .await
        .unwrap()
        .expect("offered");
    assert!(matches!(
        release_notes(&a).await,
        Err(NotesError::Invalid(_))
    ));
}
