//! Shared helpers for the updater tests. Every signing key here is generated at test time.
#![cfg(feature = "update")]
#![allow(dead_code)] // each test module uses a different subset

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;

pub struct TestKey(minisign::KeyPair);

impl TestKey {
    pub fn generate() -> TestKey {
        TestKey(minisign::KeyPair::generate_unencrypted_keypair().unwrap())
    }

    /// The public key as a `keys.txt` line: base64 of the minisign key box.
    pub fn key_line(&self) -> String {
        STANDARD.encode(self.0.pk.to_box().unwrap().to_string())
    }

    /// A `latest.json.sig` body as the release writes it: base64 of the signature box.
    pub fn sign(&self, data: &[u8]) -> String {
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

use dbento_core::update::{Product, UpdateConfig, build_config_with, target_id};
use sha2::Digest as _;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

pub const PAGE: &str = "https://example.com/download";

/// A CLI config whose manifest lives at `<base>/latest.json`, trusting `key`.
pub fn config(base: &str, key: &TestKey, current: &str) -> UpdateConfig {
    config_at(&format!("{base}/latest.json"), key, current)
}

/// A CLI config for the manifest at `url`, trusting `key`.
pub fn config_at(url: &str, key: &TestKey, current: &str) -> UpdateConfig {
    build_config_with(
        Product::Cli,
        Some(url),
        Some(PAGE),
        &key.key_line(),
        current,
    )
    .unwrap()
}

pub fn sha256_hex(data: &[u8]) -> String {
    sha2::Sha256::digest(data)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A manifest offering `version` of this host's CLI at `url` in `format`, listing `body`'s size and hash.
pub fn manifest_as(version: &str, format: &str, url: &str, body: &[u8]) -> String {
    format!(
        r#"{{"version":"{version}","targets":{{"{}":{{"url":"{url}","format":"{format}","size":{},"sha256":"{}"}}}}}}"#,
        target_id(Product::Cli).unwrap(),
        body.len(),
        sha256_hex(body)
    )
}

pub fn manifest(version: &str, url: &str, body: &[u8]) -> String {
    manifest_as(version, "binary", url, body)
}

pub async fn mount(server: &MockServer, at: &str, response: ResponseTemplate) {
    Mock::given(method("GET"))
        .and(path(at))
        .respond_with(response)
        .mount(server)
        .await;
}

/// Serves `manifest` and its signature by `key` at `/latest.json` and `/latest.json.sig`.
pub async fn publish(server: &MockServer, key: &TestKey, manifest: &str) {
    mount(
        server,
        "/latest.json",
        ResponseTemplate::new(200).set_body_string(manifest),
    )
    .await;
    let sig = key.sign(manifest.as_bytes());
    mount(
        server,
        "/latest.json.sig",
        ResponseTemplate::new(200).set_body_string(sig),
    )
    .await;
}

/// A loopback port nothing listens on.
pub fn dead_port() -> u16 {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    listener.local_addr().unwrap().port()
}

use dbento_core::update::{Available, Timeouts, check};
use std::path::Path;
use std::time::Duration;

/// Mounts `response` at `/artifact`.
pub async fn serve_artifact(server: &MockServer, response: ResponseTemplate) {
    mount(server, "/artifact", response).await;
}

/// Publishes 99.0.0 with its binary entry at `<server>/artifact` listing `listed`; returns the offer.
pub async fn offer(server: &MockServer, key: &TestKey, listed: &[u8]) -> Available {
    offer_within(server, key, listed, Duration::from_secs(15 * 60)).await
}

/// As `offer`, with the artifact's total time limit.
pub async fn offer_within(
    server: &MockServer,
    key: &TestKey,
    listed: &[u8],
    total: Duration,
) -> Available {
    let url = format!("{}/artifact", server.uri());
    publish(server, key, &manifest("99.0.0", &url, listed)).await;
    let mut t = Timeouts::default();
    t.artifact = total;
    let cfg = config(&server.uri(), key, "1.0.0").with_timeouts(t);
    check(&cfg).await.unwrap().expect("offered")
}

/// Serves `body` at `/artifact` and returns the 99.0.0 offer listing it in `format`.
pub async fn offer_as(server: &MockServer, format: &str, body: &[u8]) -> Available {
    let key = TestKey::generate();
    serve_artifact(
        server,
        ResponseTemplate::new(200).set_body_bytes(body.to_vec()),
    )
    .await;
    let url = format!("{}/artifact", server.uri());
    publish(server, &key, &manifest_as("99.0.0", format, &url, body)).await;
    check(&config(&server.uri(), &key, "1.0.0"))
        .await
        .unwrap()
        .expect("offered")
}

/// The sorted file names in `dir`.
pub fn leftovers(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}
