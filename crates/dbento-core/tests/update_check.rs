//! The check: transport, privacy, caps, version and signature failures, over a loopback server.
#![cfg(feature = "update")]

use crate::update_support::{TestKey, config, config_at, dead_port, manifest, mount, publish};
use dbento_core::update::{CheckError, Timeouts, check};
use std::time::Duration;
use wiremock::{MockServer, ResponseTemplate};

const ART: &str = "https://example.com/dbento";

async fn served() -> (MockServer, TestKey) {
    (MockServer::start().await, TestKey::generate())
}

#[tokio::test]
async fn a_newer_signed_release_is_offered() {
    let (s, k) = served().await;
    publish(&s, &k, &manifest("2.0.0", ART, b"new")).await;
    let a = check(&config(&s.uri(), &k, "1.0.0"))
        .await
        .unwrap()
        .expect("offered");
    assert_eq!((a.version(), a.size(), a.url()), ("2.0.0", 3, ART));
}

#[tokio::test]
async fn the_check_sends_only_the_private_header_set() {
    let (s, k) = served().await;
    publish(&s, &k, &manifest("2.0.0", ART, b"new")).await;
    check(&config(&s.uri(), &k, "1.0.0")).await.unwrap();
    let requests = s.received_requests().await.unwrap();
    let paths: Vec<&str> = requests.iter().map(|r| r.url.path()).collect();
    assert_eq!(paths, ["/latest.json", "/latest.json.sig"]);
    for r in &requests {
        let mut names: Vec<&str> = r.headers.keys().map(|n| n.as_str()).collect();
        names.sort_unstable();
        assert_eq!(
            names,
            ["accept", "accept-encoding", "host", "user-agent"],
            "{}",
            r.url
        );
        let ua = format!("dbento/{}", env!("CARGO_PKG_VERSION"));
        assert_eq!(r.headers["user-agent"], ua.as_str());
        assert_eq!(r.headers["accept"], "*/*");
        assert_eq!(r.headers["accept-encoding"], "identity");
        assert_eq!(r.url.query(), None);
    }
}

#[tokio::test]
async fn a_replayed_or_equal_release_offers_nothing() {
    for old in ["1.0.0", "0.9.0"] {
        let (s, k) = served().await;
        publish(&s, &k, &manifest(old, ART, b"x")).await;
        let got = check(&config(&s.uri(), &k, "1.0.0")).await.unwrap();
        assert_eq!(got, None, "{old}");
    }
}

#[tokio::test]
async fn a_manifest_edited_after_signing_is_refused() {
    let (s, k) = served().await;
    let signed = manifest("2.0.0", ART, b"new");
    let edited = signed.replace("2.0.0", "3.0.0");
    mount(
        &s,
        "/latest.json",
        ResponseTemplate::new(200).set_body_string(edited),
    )
    .await;
    let sig = k.sign(signed.as_bytes());
    mount(
        &s,
        "/latest.json.sig",
        ResponseTemplate::new(200).set_body_string(sig),
    )
    .await;
    let got = check(&config(&s.uri(), &k, "1.0.0")).await;
    assert!(
        matches!(got, Err(CheckError::ManifestSignature(_))),
        "{got:?}"
    );
}

#[tokio::test]
async fn a_badly_signed_body_is_never_parsed() {
    let (s, k) = served().await;
    mount(
        &s,
        "/latest.json",
        ResponseTemplate::new(200).set_body_string("this is not json"),
    )
    .await;
    let sig = k.sign(b"something else");
    mount(
        &s,
        "/latest.json.sig",
        ResponseTemplate::new(200).set_body_string(sig),
    )
    .await;
    let got = check(&config(&s.uri(), &k, "1.0.0")).await;
    assert!(
        matches!(got, Err(CheckError::ManifestSignature(_))),
        "a parse error would mean parsing came first: {got:?}"
    );
}

#[tokio::test]
async fn a_missing_signature_is_a_signature_error() {
    let (s, k) = served().await;
    let body = manifest("2.0.0", ART, b"x");
    mount(
        &s,
        "/latest.json",
        ResponseTemplate::new(200).set_body_string(body),
    )
    .await;
    mount(&s, "/latest.json.sig", ResponseTemplate::new(404)).await;
    let got = check(&config(&s.uri(), &k, "1.0.0")).await;
    assert!(
        matches!(got, Err(CheckError::ManifestSignature(_))),
        "{got:?}"
    );
}

#[tokio::test]
async fn a_release_signed_by_another_key_is_unknown() {
    let (s, signer) = served().await;
    publish(&s, &signer, &manifest("2.0.0", ART, b"x")).await;
    let trusted = TestKey::generate();
    assert_eq!(
        check(&config(&s.uri(), &trusted, "1.0.0"))
            .await
            .unwrap_err(),
        CheckError::UnknownKey
    );
}

#[tokio::test]
async fn a_signed_pre_release_is_invalid() {
    let (s, k) = served().await;
    publish(&s, &k, &manifest("2.0.0-rc.1", ART, b"x")).await;
    let got = check(&config(&s.uri(), &k, "1.0.0")).await;
    assert!(
        matches!(got, Err(CheckError::ManifestInvalid(_))),
        "{got:?}"
    );
}

#[tokio::test]
async fn a_release_without_a_manifest_and_a_server_error_are_http() {
    let (s, k) = served().await;
    assert_eq!(
        check(&config(&s.uri(), &k, "1.0.0")).await.unwrap_err(),
        CheckError::Http(404)
    );
    mount(&s, "/latest.json", ResponseTemplate::new(500)).await;
    assert_eq!(
        check(&config(&s.uri(), &k, "1.0.0")).await.unwrap_err(),
        CheckError::Http(500)
    );
}

#[tokio::test]
async fn oversized_bodies_are_refused() {
    let (s, k) = served().await;
    mount(
        &s,
        "/latest.json",
        ResponseTemplate::new(200).set_body_bytes(vec![b' '; 64 * 1024 + 1]),
    )
    .await;
    let got = check(&config(&s.uri(), &k, "1.0.0")).await;
    assert!(
        matches!(got, Err(CheckError::ManifestInvalid(_))),
        "manifest: {got:?}"
    );

    let (s, k) = served().await;
    let body = manifest("2.0.0", ART, b"x");
    mount(
        &s,
        "/latest.json",
        ResponseTemplate::new(200).set_body_string(body),
    )
    .await;
    mount(
        &s,
        "/latest.json.sig",
        ResponseTemplate::new(200).set_body_bytes(vec![b'A'; 4097]),
    )
    .await;
    let got = check(&config(&s.uri(), &k, "1.0.0")).await;
    assert!(
        matches!(got, Err(CheckError::ManifestInvalid(_))),
        "signature: {got:?}"
    );
}

#[tokio::test]
async fn an_unreachable_server_is_offline() {
    let base = format!("http://127.0.0.1:{}", dead_port());
    let got = check(&config(&base, &TestKey::generate(), "1.0.0")).await;
    assert!(matches!(got, Err(CheckError::Offline(_))), "{got:?}");
}

#[tokio::test]
async fn a_slow_manifest_times_out() {
    let (s, k) = served().await;
    let slow = ResponseTemplate::new(200)
        .set_body_string("{}")
        .set_delay(Duration::from_secs(5));
    mount(&s, "/latest.json", slow).await;
    let mut short = Timeouts::default();
    short.manifest = Duration::from_millis(200);
    let cfg = config(&s.uri(), &k, "1.0.0").with_timeouts(short);
    assert_eq!(check(&cfg).await.unwrap_err(), CheckError::Timeout);
}

#[tokio::test]
async fn redirects_are_followed_within_the_rules() {
    let (s, k) = served().await;
    publish(&s, &k, &manifest("2.0.0", ART, b"x")).await;
    // Two hops, as GitHub's /latest/download/ answers.
    for (from, to) in [("a", "b/"), ("b", "")] {
        for file in ["latest.json", "latest.json.sig"] {
            let hop = ResponseTemplate::new(302)
                .insert_header("location", format!("{}/{to}{file}", s.uri()));
            mount(&s, &format!("/{from}/{file}"), hop).await;
        }
    }
    let cfg = config_at(&format!("{}/a/latest.json", s.uri()), &k, "1.0.0");
    assert!(check(&cfg).await.unwrap().is_some());
    let requests = s.received_requests().await.unwrap();
    for r in &requests {
        assert!(
            r.headers.get("referer").is_none(),
            "{} sent a referer",
            r.url
        );
    }

    // Under a loopback override the https rule is off (the signature protects); an unresolvable hop is Offline.
    let off =
        ResponseTemplate::new(302).insert_header("location", "http://example.invalid/latest.json");
    mount(&s, "/insecure/latest.json", off).await;
    let cfg = config_at(&format!("{}/insecure/latest.json", s.uri()), &k, "1.0.0");
    assert!(matches!(check(&cfg).await, Err(CheckError::Offline(_))));

    // Six hops is one too many.
    for i in 0..6 {
        let next = format!("{}/hop{}/latest.json", s.uri(), i + 1);
        let hop = ResponseTemplate::new(302).insert_header("location", next);
        mount(&s, &format!("/hop{i}/latest.json"), hop).await;
    }
    let cfg = config_at(&format!("{}/hop0/latest.json", s.uri()), &k, "1.0.0");
    assert!(matches!(check(&cfg).await, Err(CheckError::Offline(_))));
}
