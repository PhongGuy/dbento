//! S3 harness: every `S3Moto` is a fresh bucket on a Moto server, seeded with the keys the
//! constants describe. Buckets are unique per handle, so the handles of one process share one
//! server and never see each other's keys.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::time::{SystemTime, UNIX_EPOCH};

use aws_sdk_s3::Client;
use aws_sdk_s3::config::{
    BehaviorVersion, Credentials, Region, RequestChecksumCalculation, ResponseChecksumValidation,
};
use aws_sdk_s3::primitives::ByteStream;
use aws_smithy_http_client::tls::{self, rustls_provider::CryptoMode};
use tokio::sync::{Mutex, Semaphore};
use tokio::task::JoinSet;

use crate::moto::{DySource, Moto, dy_source, moto};

/// The environment variable the generated connection reads its secret key from.
pub const S3_SECRET_VAR: &str = "DBENTO_TEST_S3_SECRET";
/// The access key every handle signs with; Moto accepts any pair.
pub const S3_ACCESS_KEY: &str = "AKIDDBENTOS3TEST";
/// The secret key paired with `S3_ACCESS_KEY`; set it in `S3_SECRET_VAR` for a connection.
pub const S3_SECRET: &str = "dbento-moto-s3-secret";
/// The region every handle's client and connection name.
pub const S3_REGION: &str = "us-east-1";
/// Keys `many/00000` to `many/02499` that `seed_paged` writes, each with body `"p"`.
pub const PAGED: usize = 2_500;
/// The size of `big/three-mib.bin`, whose byte `i` is `big_byte(i)`.
pub const THREE_MIB: usize = 3 << 20;
/// Every key `s3_moto` seeds.
pub const BASE_KEYS: usize = 18;
/// Keys whose bytes a shell or a file system could misread, each with body `"x"`.
pub const HOSTILE_KEYS: [&str; 9] = [
    "hostile/tab\tname",
    "hostile/soh\u{1}x",
    "hostile/esc\u{1b}[31mred",
    "hostile/nel\u{85}x",
    "hostile/bidi\u{202e}txt.exe",
    "hostile/dot/../escape",
    "hostile/quote'and;semi",
    "hostile/plus+and space",
    "hostile/pct%41",
];

const S3_URL_VAR: &str = "DBENTO_S3_URL";
/// PutObject calls `seed_paged` keeps in flight.
const PAGED_IN_FLIGHT: usize = 32;

static NEXT_BUCKET: AtomicU64 = AtomicU64::new(0);
/// The container the live handles of this process share, gone once the last one drops.
static SHARED: Mutex<Weak<Moto>> = Mutex::const_new(Weak::new());

/// Byte `i` of `big/three-mib.bin`: a pattern that a shifted or truncated read never matches.
pub fn big_byte(i: usize) -> u8 {
    (i * 31 % 251) as u8
}

/// A non-empty `DBENTO_S3_URL` wins, then the DynamoDB server, since one Moto serves both.
pub fn s3_source(env: impl Fn(&str) -> Option<String>) -> DySource {
    match env(S3_URL_VAR).filter(|u| !u.is_empty()) {
        Some(url) => DySource::Url(url),
        None => dy_source(env),
    }
}

/// A seeded bucket. One holding a container must be dropped inside a tokio runtime.
pub struct S3Moto {
    client: Client,
    bucket: String,
    server: Arc<Moto>,
}

impl S3Moto {
    /// The server's `http://host:port`.
    pub fn endpoint(&self) -> &str {
        self.server.endpoint()
    }
    /// This handle's bucket.
    pub fn bucket(&self) -> &str {
        &self.bucket
    }
    /// A path-style client in `S3_REGION`, signed with the harness keys.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// `[connections.<name>]`: engine, region, endpoint, access_key_id,
    /// `secret = { env = S3_SECRET_VAR }`, `buckets = [<bucket>]`, then `extra` on its own line.
    pub fn connection_toml(&self, name: &str, extra: &str) -> String {
        let (endpoint, bucket) = (self.endpoint(), &self.bucket);
        format!(
            "[connections.{name}]\nengine = \"s3\"\nregion = \"{S3_REGION}\"\n\
             endpoint = \"{endpoint}\"\naccess_key_id = \"{S3_ACCESS_KEY}\"\n\
             secret = {{ env = \"{S3_SECRET_VAR}\" }}\nbuckets = [\"{bucket}\"]\n{extra}\n"
        )
    }

    /// Writes `body` at `key` in this handle's bucket.
    pub async fn put(&self, key: &str, body: Vec<u8>, content_type: Option<&str>) {
        put(&self.client, &self.bucket, key, body, content_type).await;
    }

    /// Writes the `PAGED` keys, a bounded number of PUTs in flight at once.
    pub async fn seed_paged(&self) {
        let slots = Arc::new(Semaphore::new(PAGED_IN_FLIGHT));
        let mut puts = JoinSet::new();
        for i in 0..PAGED {
            let permit = Arc::clone(&slots)
                .acquire_owned()
                .await
                .expect("the seed semaphore is never closed");
            let (client, bucket) = (self.client.clone(), self.bucket.clone());
            puts.spawn(async move {
                put(
                    &client,
                    &bucket,
                    &format!("many/{i:05}"),
                    b"p".to_vec(),
                    None,
                )
                .await;
                drop(permit);
            });
        }
        while let Some(done) = puts.join_next().await {
            done.unwrap_or_else(|e| panic!("a paged seed PUT failed: {e}"));
        }
    }

    async fn seed(&self) {
        let seed: [(&str, Vec<u8>, Option<&str>); 9] = [
            (
                "text/hello.txt",
                b"hello, bento\n".to_vec(),
                Some("text/plain"),
            ),
            (
                "json/doc.json",
                br#"{"name":"bento","tiers":3}"#.to_vec(),
                Some("application/json"),
            ),
            (
                "bin/blob.bin",
                (0..=255u8).collect(),
                Some("application/octet-stream"),
            ),
            (
                "big/three-mib.bin",
                (0..THREE_MIB).map(big_byte).collect(),
                None,
            ),
            ("empty/zero", Vec::new(), None),
            ("deep/a/top.txt", b"top".to_vec(), None),
            ("deep/a/b/c/d/e/leaf.txt", b"leaf".to_vec(), None),
            ("unicode/日本語/ファイル.txt", b"jp".to_vec(), None),
            ("unicode/émoji-🍱.json", b"{}".to_vec(), None),
        ];
        for (key, body, content_type) in seed {
            self.put(key, body, content_type).await;
        }
        for key in HOSTILE_KEYS {
            self.put(key, b"x".to_vec(), None).await;
        }
    }
}

async fn put(client: &Client, bucket: &str, key: &str, body: Vec<u8>, content_type: Option<&str>) {
    client
        .put_object()
        .bucket(bucket)
        .key(key)
        .body(ByteStream::from(body))
        .set_content_type(content_type.map(str::to_owned))
        .send()
        .await
        .unwrap_or_else(|e| panic!("cannot put {key:?}: {e:?}"));
}

/// A client for `endpoint` in path style, with checksums only where S3 requires them, as a
/// third-party store gets.
fn client(endpoint: &str) -> Client {
    let http = aws_smithy_http_client::Builder::new()
        .tls_provider(tls::Provider::Rustls(CryptoMode::Ring))
        .build_https();
    let conf = aws_sdk_s3::Config::builder()
        .behavior_version(BehaviorVersion::v2026_11_10())
        .region(Region::new(S3_REGION))
        .endpoint_url(endpoint)
        .force_path_style(true)
        .request_checksum_calculation(RequestChecksumCalculation::WhenRequired)
        .response_checksum_validation(ResponseChecksumValidation::WhenRequired)
        .credentials_provider(Credentials::new(
            S3_ACCESS_KEY,
            S3_SECRET,
            None,
            None,
            "dbento-testkit",
        ))
        .http_client(http)
        .build();
    Client::from_conf(conf)
}

/// `dbento-<pid>-<n>-<hex nanos>`: lowercase, digits and dashes, well under 63 characters.
fn unique_bucket() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    format!(
        "dbento-{}-{}-{nanos:x}",
        std::process::id(),
        NEXT_BUCKET.fetch_add(1, Ordering::Relaxed)
    )
}

/// The URL server, or the container this process's live handles share. A test holding two
/// handles then takes one container slot, never two, so it cannot deadlock against another
/// test that waits on the second slot.
async fn server() -> Arc<Moto> {
    match s3_source(|k| std::env::var(k).ok()) {
        DySource::Url(url) => Arc::new(moto(DySource::Url(url)).await),
        DySource::Container => {
            // Held across the start, so concurrent first handles start one container, not two.
            let mut shared = SHARED.lock().await;
            if let Some(server) = shared.upgrade() {
                return server;
            }
            let server = Arc::new(moto(DySource::Container).await);
            *shared = Arc::downgrade(&server);
            server
        }
    }
}

/// A fresh bucket on the URL server or on this process's shared container, with the base seed
/// written; see the constants for the seed.
pub async fn s3_moto() -> S3Moto {
    let server = server().await;
    let handle = S3Moto {
        client: client(server.endpoint()),
        bucket: unique_bucket(),
        server,
    };
    handle
        .client
        .create_bucket()
        .bucket(&handle.bucket)
        .send()
        .await
        .unwrap_or_else(|e| panic!("cannot create bucket {}: {e:?}", handle.bucket));
    handle.seed().await;
    handle
}
