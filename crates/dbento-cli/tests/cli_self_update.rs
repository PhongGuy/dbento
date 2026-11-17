//! `dbento self-update` against a loopback feed: exit codes, `--check`, an unconfigured build
//! and the refusal to replace a build from source. The test binary never replaces itself.
use crate::common::dbento;
use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use dbento_core::update::{DOWNLOAD_PAGE, Product, release_notes_url, target_id};
use std::process::Output;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

struct Key(minisign::KeyPair);
impl Key {
    fn generate() -> Key {
        Key(minisign::KeyPair::generate_unencrypted_keypair().unwrap())
    }
    /// The public key as a `keys.txt` line.
    fn line(&self) -> String {
        STANDARD.encode(self.0.pk.to_box().unwrap().to_string())
    }
    /// A `latest.json.sig` body as the release writes it.
    fn sign(&self, data: &[u8]) -> String {
        let trusted = Some("timestamp:0\tfile:latest.json");
        let sig = minisign::sign(
            Some(&self.0.pk),
            &self.0.sk,
            std::io::Cursor::new(data),
            trusted,
            None,
        );
        STANDARD.encode(sig.unwrap().to_string())
    }
}

/// Serves a manifest offering `version` of this host's CLI, signed by `key`.
async fn feed(key: &Key, version: &str) -> MockServer {
    let server = MockServer::start().await;
    let manifest = format!(
        r#"{{"version":"{version}","targets":{{"{}":{{"url":"{}/artifact","format":"binary","size":1,"sha256":"{}"}}}}}}"#,
        target_id(Product::Cli).unwrap(),
        server.uri(),
        "0".repeat(64)
    );
    let sig = key.sign(manifest.as_bytes());
    for (at, body) in [("/latest.json", manifest), ("/latest.json.sig", sig)] {
        Mock::given(method("GET"))
            .and(path(at))
            .respond_with(ResponseTemplate::new(200).set_body_string(body))
            .mount(&server)
            .await;
    }
    server
}

/// Runs `dbento self-update <args>` off the runtime thread, so the loopback server keeps answering.
async fn self_update(server: Option<&MockServer>, key: Option<&Key>, args: &[&str]) -> Output {
    let mut cmd = dbento();
    cmd.env_remove("DBENTO_UPDATE_MANIFEST_URL")
        .env_remove("DBENTO_TEST_UPDATE_KEYS")
        .env("DBENTO_TEST_NO_KEYCHAIN", "1")
        .arg("self-update")
        .args(args);
    if let Some(s) = server {
        cmd.env(
            "DBENTO_UPDATE_MANIFEST_URL",
            format!("{}/latest.json", s.uri()),
        );
    }
    if let Some(k) = key {
        cmd.env("DBENTO_TEST_UPDATE_KEYS", k.line());
    }
    tokio::task::spawn_blocking(move || cmd.output().unwrap())
        .await
        .unwrap()
}

fn text(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).into_owned()
}

async fn artifact_requests(s: &MockServer) -> usize {
    let all = s.received_requests().await.unwrap();
    all.iter().filter(|r| r.url.path() == "/artifact").count()
}

#[tokio::test]
async fn a_build_without_a_feed_fails_with_code_1() {
    let out = self_update(None, None, &[]).await;
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(text(&out.stderr).contains("not configured"), "{out:?}");
}

#[tokio::test]
async fn an_older_release_is_up_to_date() {
    let key = Key::generate();
    let s = feed(&key, "0.0.1").await;
    for args in [&[][..], &["--check"][..]] {
        let out = self_update(Some(&s), Some(&key), args).await;
        assert_eq!(out.status.code(), Some(0), "{args:?}: {out:?}");
        let up = format!("dbento {} is up to date", env!("CARGO_PKG_VERSION"));
        assert!(text(&out.stdout).contains(&up), "{out:?}");
    }
}

#[tokio::test]
async fn a_release_without_a_manifest_is_up_to_date() {
    let s = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/latest.json"))
        .respond_with(ResponseTemplate::new(404))
        .mount(&s)
        .await;
    for args in [&[][..], &["--check"][..]] {
        let out = self_update(Some(&s), Some(&Key::generate()), args).await;
        assert_eq!(out.status.code(), Some(0), "{args:?}: {out:?}");
        assert!(text(&out.stdout).contains("is up to date"), "{out:?}");
    }
}

#[tokio::test]
async fn check_reports_a_newer_release_with_code_2() {
    let key = Key::generate();
    let s = feed(&key, "99.0.0").await;
    let out = self_update(Some(&s), Some(&key), &["--check"]).await;
    assert_eq!(out.status.code(), Some(2), "{out:?}");
    let stdout = text(&out.stdout);
    assert!(stdout.contains("dbento 99.0.0 is available"), "{stdout}");
    assert!(stdout.contains(&release_notes_url("99.0.0")), "{stdout}");
    assert_eq!(artifact_requests(&s).await, 0);
}

#[tokio::test]
async fn a_build_from_source_is_never_replaced() {
    let key = Key::generate();
    let s = feed(&key, "99.0.0").await;
    let bin = assert_cmd::cargo::cargo_bin("dbento");
    let before = std::fs::metadata(&bin).unwrap().modified().unwrap();
    for args in [&[][..], &["--yes"][..]] {
        let out = self_update(Some(&s), Some(&key), args).await;
        assert_eq!(out.status.code(), Some(0), "{args:?}: {out:?}");
        let stdout = text(&out.stdout);
        assert!(stdout.contains("built from source"), "{stdout}");
        assert!(stdout.contains(DOWNLOAD_PAGE), "{stdout}");
    }
    assert_eq!(artifact_requests(&s).await, 0);
    assert_eq!(std::fs::metadata(&bin).unwrap().modified().unwrap(), before);
}

#[tokio::test]
async fn a_manifest_signed_by_another_key_fails_with_code_1() {
    let (signer, trusted) = (Key::generate(), Key::generate());
    let s = feed(&signer, "99.0.0").await;
    let out = self_update(Some(&s), Some(&trusted), &["--check"]).await;
    assert_eq!(out.status.code(), Some(1), "{out:?}");
    assert!(text(&out.stderr).contains("unknown key"), "{out:?}");
}
