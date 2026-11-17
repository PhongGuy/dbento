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
