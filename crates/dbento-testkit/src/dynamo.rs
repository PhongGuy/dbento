//! DynamoDB harness: every `DynamoDb` is a seeded region leased on a capped Moto container or on
//! the server `DBENTO_DYNAMO_URL` names. Moto keeps tables per region, so leases never collide.

use std::collections::{BTreeSet, HashMap};
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use aws_sdk_dynamodb::Client;
use aws_sdk_dynamodb::config::{BehaviorVersion, Credentials, Region};
use aws_sdk_dynamodb::primitives::Blob;
use aws_sdk_dynamodb::types::{
    AttributeDefinition, AttributeValue, BillingMode, GlobalSecondaryIndex, KeySchemaElement,
    KeyType, LocalSecondaryIndex, Projection, ProjectionType, PutRequest, ScalarAttributeType,
    Select, WriteRequest,
};
use aws_smithy_http_client::tls::{self, rustls_provider::CryptoMode};
use testcontainers_modules::testcontainers::core::ContainerPort;
use testcontainers_modules::testcontainers::{
    ContainerAsync, GenericImage, ImageExt, runners::AsyncRunner,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use super::{ATTEMPT_LABEL, NEXT_ATTEMPT, engine, remove_labelled, retry_start, sweep_orphans};

/// The environment variable the generated connection reads its secret key from.
pub const DY_SECRET_VAR: &str = "DBENTO_TEST_DY_SECRET";
/// The access key and secret every handle signs with; Moto accepts any pair.
pub const DY_ACCESS_KEY: &str = "AKIDDBENTOTEST";
pub const DY_SECRET: &str = "dbento-moto-secret";
/// The gate image's tag.
pub const MOTO_TAG: &str = "5.2.5";
/// Items in `orders`.
pub const ORDERS: usize = 300;
/// The regions a handle may lease, one handle per region at a time.
pub const REGIONS: [&str; 16] = [
    "us-east-1",
    "us-east-2",
    "us-west-1",
    "us-west-2",
    "ca-central-1",
    "eu-west-1",
    "eu-west-2",
    "eu-west-3",
    "eu-central-1",
    "eu-north-1",
    "ap-south-1",
    "ap-northeast-1",
    "ap-northeast-2",
    "ap-southeast-1",
    "ap-southeast-2",
    "sa-east-1",
];

const URL_VAR: &str = "DBENTO_DYNAMO_URL";
const MOTO_PORT: u16 = 5000;
const READY_TIMEOUT: Duration = Duration::from_secs(60);

/// Where the DynamoDB tests get their server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DySource {
    /// A running Moto at this endpoint.
    Url(String),
    /// A container of the gate image.
    Container,
}

/// A non-empty `DBENTO_DYNAMO_URL` wins; anything else means a container.
pub fn dy_source(env: impl Fn(&str) -> Option<String>) -> DySource {
    env(URL_VAR)
        .filter(|u| !u.is_empty())
        .map_or(DySource::Container, DySource::Url)
}

/// Takes the lowest region index below `REGIONS.len()` missing from `taken`.
pub fn lease_from(taken: &mut BTreeSet<usize>) -> Option<usize> {
    let free = (0..REGIONS.len()).find(|i| !taken.contains(i))?;
    taken.insert(free);
    Some(free)
}

fn leases() -> &'static Mutex<BTreeSet<usize>> {
    static LEASES: Mutex<BTreeSet<usize>> = Mutex::new(BTreeSet::new());
    &LEASES
}

/// A leased region index, given back when dropped, even by a handle that failed to seed.
struct Lease(usize);

impl Lease {
    fn take() -> Self {
        let mut taken = leases().lock().unwrap_or_else(|p| p.into_inner());
        Self(lease_from(&mut taken).expect("all 16 DynamoDB test regions are leased"))
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        leases()
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&self.0);
    }
}

/// A seeded region. One holding a container must be dropped inside a tokio runtime.
pub struct DynamoDb {
    endpoint: String,
    client: Client,
    // Declared before the permit, so the container is gone before another may start.
    container: Option<ContainerAsync<GenericImage>>,
    _permit: Option<OwnedSemaphorePermit>,
    lease: Lease,
}

impl DynamoDb {
    /// The server's `http://host:port`.
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }
    /// The leased region the seed lives in.
    pub fn region(&self) -> &str {
        REGIONS[self.lease.0]
    }
    /// A native client signed for this handle's region.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// `[connections.<name>]`: engine, region, endpoint, access_key_id,
    /// `secret = { env = DY_SECRET_VAR }`, then `extra` on its own line.
    pub fn connection_toml(&self, name: &str, extra: &str) -> String {
        let (region, endpoint) = (self.region(), &self.endpoint);
        format!(
            "[connections.{name}]\nengine = \"dynamodb\"\nregion = \"{region}\"\n\
             endpoint = \"{endpoint}\"\naccess_key_id = \"{DY_ACCESS_KEY}\"\n\
             secret = {{ env = \"{DY_SECRET_VAR}\" }}\n{extra}\n"
        )
    }

    /// Items in `table`, by native Scan with Select COUNT across pages.
    pub async fn count(&self, table: &str) -> usize {
        let mut total = 0;
        let mut start = None;
        loop {
            let page = self
                .client
                .scan()
                .table_name(table)
                .select(Select::Count)
                .set_exclusive_start_key(start)
                .send()
                .await
                .unwrap_or_else(|e| panic!("cannot count {table}: {e:?}"));
            total += usize::try_from(page.count()).expect("a page count is never negative");
            start = page.last_evaluated_key;
            if start.is_none() {
                return total;
            }
        }
    }

    /// Drops every table in the region, then creates and fills `h`, `orders` and `types` in
    /// that order, which is the order Moto lists them in.
    async fn seed(&self) {
        let c = &self.client;
        let listed = c
            .list_tables()
            .send()
            .await
            .unwrap_or_else(|e| panic!("cannot list the Moto tables: {e:?}"));
        for table in listed.table_names() {
            c.delete_table()
                .table_name(table)
                .send()
                .await
                .unwrap_or_else(|e| panic!("cannot drop {table}: {e:?}"));
        }
        c.create_table()
            .table_name("h")
            .billing_mode(BillingMode::PayPerRequest)
            .attribute_definitions(attribute("id", ScalarAttributeType::S))
            .key_schema(key("id", KeyType::Hash))
            .send()
            .await
            .unwrap_or_else(|e| panic!("cannot create h: {e:?}"));
        c.create_table()
            .table_name("orders")
            .billing_mode(BillingMode::PayPerRequest)
            .attribute_definitions(attribute("pk", ScalarAttributeType::S))
            .attribute_definitions(attribute("sk", ScalarAttributeType::N))
            .attribute_definitions(attribute("status", ScalarAttributeType::S))
            .attribute_definitions(attribute("created", ScalarAttributeType::N))
            .key_schema(key("pk", KeyType::Hash))
            .key_schema(key("sk", KeyType::Range))
            .global_secondary_indexes(
                GlobalSecondaryIndex::builder()
                    .index_name("by_status")
                    .key_schema(key("status", KeyType::Hash))
                    .key_schema(key("created", KeyType::Range))
                    .projection(all())
                    .build()
                    .expect("the GSI names every required field"),
            )
            .local_secondary_indexes(
                LocalSecondaryIndex::builder()
                    .index_name("by_created")
                    .key_schema(key("pk", KeyType::Hash))
                    .key_schema(key("created", KeyType::Range))
                    .projection(all())
                    .build()
                    .expect("the LSI names every required field"),
            )
            .send()
            .await
            .unwrap_or_else(|e| panic!("cannot create orders: {e:?}"));
        c.create_table()
            .table_name("types")
            .billing_mode(BillingMode::PayPerRequest)
            .attribute_definitions(attribute("id", ScalarAttributeType::S))
            .key_schema(key("id", KeyType::Hash))
            .send()
            .await
            .unwrap_or_else(|e| panic!("cannot create types: {e:?}"));
        self.fill_orders().await;
        c.put_item()
            .table_name("types")
            .set_item(Some(type_item()))
            .send()
            .await
            .unwrap_or_else(|e| panic!("cannot seed types: {e:?}"));
    }

    /// Writes the `ORDERS` items in batches of 25, the BatchWriteItem maximum, resending
    /// anything the server leaves unprocessed.
    async fn fill_orders(&self) {
        let pad = "x".repeat(10_000);
        let items: Vec<WriteRequest> = (0..ORDERS)
            .map(|i| {
                let status = if i % 3 == 0 { "open" } else { "closed" };
                let item = HashMap::from([
                    ("pk".to_owned(), AttributeValue::S(format!("cust{}", i % 5))),
                    ("sk".to_owned(), AttributeValue::N(i.to_string())),
                    ("status".to_owned(), AttributeValue::S(status.to_owned())),
                    (
                        "created".to_owned(),
                        AttributeValue::N((1000 + i).to_string()),
                    ),
                    ("pad".to_owned(), AttributeValue::S(pad.clone())),
                ]);
                let put = PutRequest::builder()
                    .set_item(Some(item))
                    .build()
                    .expect("the put names its item");
                WriteRequest::builder().put_request(put).build()
            })
            .collect();
        for chunk in items.chunks(25) {
            let mut pending = HashMap::from([("orders".to_owned(), chunk.to_vec())]);
            while !pending.is_empty() {
                let out = self
                    .client
                    .batch_write_item()
                    .set_request_items(Some(pending))
                    .send()
                    .await
                    .unwrap_or_else(|e| panic!("cannot seed orders: {e:?}"));
                pending = out.unprocessed_items.unwrap_or_default();
                pending.retain(|_, writes| !writes.is_empty());
            }
        }
    }
}

impl Drop for DynamoDb {
    // Runs on its own thread and runtime and never panics. A container is killed; a URL server
    // keeps the tables, which the region's next holder drops.
    fn drop(&mut self) {
        let Some(id) = self.container.as_ref().map(|c| c.id().to_owned()) else {
            return;
        };
        let done = std::thread::spawn(move || -> Result<(), String> {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|e| e.to_string())?;
            runtime.block_on(async {
                engine()?
                    .kill_container(&id, None)
                    .await
                    .map_err(|e| format!("cannot kill container {id}: {e}"))
            })
        })
        .join();
        match done {
            Ok(Ok(())) => {}
            Ok(Err(e)) => eprintln!("dbento-testkit: cannot drop the Moto container: {e}"),
            Err(_) => eprintln!("dbento-testkit: dropping the Moto container panicked"),
        }
    }
}

fn attribute(name: &str, kind: ScalarAttributeType) -> AttributeDefinition {
    AttributeDefinition::builder()
        .attribute_name(name)
        .attribute_type(kind)
        .build()
        .expect("the attribute names its name and type")
}

fn key(name: &str, kind: KeyType) -> KeySchemaElement {
    KeySchemaElement::builder()
        .attribute_name(name)
        .key_type(kind)
        .build()
        .expect("the key names its attribute and type")
}

fn all() -> Projection {
    Projection::builder()
        .projection_type(ProjectionType::All)
        .build()
}

/// One attribute of every DynamoDB type, keyed `id = "t1"`.
fn type_item() -> HashMap<String, AttributeValue> {
    let s = |v: &str| AttributeValue::S(v.to_owned());
    let n = |v: &str| AttributeValue::N(v.to_owned());
    HashMap::from([
        ("id".to_owned(), s("t1")),
        ("s".to_owned(), s("text")),
        ("n".to_owned(), n("12345678901234567890123456789012345678")),
        (
            "b".to_owned(),
            AttributeValue::B(Blob::new(vec![0u8, 255, 254])),
        ),
        ("bool".to_owned(), AttributeValue::Bool(true)),
        ("nul".to_owned(), AttributeValue::Null(true)),
        (
            "m".to_owned(),
            AttributeValue::M(HashMap::from([("k".to_owned(), n("1"))])),
        ),
        (
            "l".to_owned(),
            AttributeValue::L(vec![AttributeValue::Null(true), s("a")]),
        ),
        (
            "ss".to_owned(),
            AttributeValue::Ss(vec!["x".to_owned(), "y".to_owned()]),
        ),
        (
            "ns".to_owned(),
            AttributeValue::Ns(vec!["1".to_owned(), "2".to_owned()]),
        ),
        (
            "bs".to_owned(),
            AttributeValue::Bs(vec![Blob::new(vec![1u8]), Blob::new(vec![2u8])]),
        ),
    ])
}

/// A client for `endpoint` in `region`, signed with the harness keys.
fn client(endpoint: &str, region: &str) -> Client {
    let http = aws_smithy_http_client::Builder::new()
        .tls_provider(tls::Provider::Rustls(CryptoMode::Ring))
        .build_https();
    let conf = aws_sdk_dynamodb::Config::builder()
        .behavior_version(BehaviorVersion::v2026_11_10())
        .region(Region::new(region.to_owned()))
        .endpoint_url(endpoint)
        .credentials_provider(Credentials::new(
            DY_ACCESS_KEY,
            DY_SECRET,
            None,
            None,
            "dbento-testkit",
        ))
        .http_client(http)
        .build();
    Client::from_conf(conf)
}

/// A region leased on the URL server or on a fresh container, its old tables dropped and the
/// seed written; see the constants for the seed.
pub async fn dynamo_db() -> DynamoDb {
    let (endpoint, container, permit) = match dy_source(|k| std::env::var(k).ok()) {
        DySource::Url(url) => (url, None, None),
        DySource::Container => {
            let (endpoint, c, permit) = container().await;
            (endpoint, Some(c), Some(permit))
        }
    };
    let lease = Lease::take();
    let db = DynamoDb {
        client: client(&endpoint, REGIONS[lease.0]),
        endpoint,
        container,
        _permit: permit,
        lease,
    };
    db.seed().await;
    db
}

/// Two Moto containers at a time in this process.
fn slots() -> Arc<Semaphore> {
    static SLOTS: OnceLock<Arc<Semaphore>> = OnceLock::new();
    Arc::clone(SLOTS.get_or_init(|| Arc::new(Semaphore::new(2))))
}

/// Starts the gate image labelled and memory-capped, retried when its host port turns out to be
/// taken, and waits until it answers.
async fn container() -> (String, ContainerAsync<GenericImage>, OwnedSemaphorePermit) {
    let permit = slots()
        .acquire_owned()
        .await
        .expect("the container semaphore is never closed");
    sweep_orphans().await;
    let c = retry_start(|_| {
        let attempt = format!(
            "{}-{}",
            std::process::id(),
            NEXT_ATTEMPT.fetch_add(1, Ordering::Relaxed)
        );
        let request = GenericImage::new("motoserver/moto", MOTO_TAG)
            .with_exposed_port(ContainerPort::Tcp(MOTO_PORT))
            .with_host_config_modifier(|h| h.memory = Some(512 << 20))
            .with_label("dbento.role", "test")
            .with_label("dbento.pid", std::process::id().to_string())
            .with_label(ATTEMPT_LABEL, attempt.clone());
        async move {
            match request.start().await {
                Ok(container) => Ok(container),
                Err(e) => {
                    remove_labelled(&format!("{ATTEMPT_LABEL}={attempt}")).await;
                    Err(e.to_string())
                }
            }
        }
    })
    .await
    .unwrap_or_else(|e| {
        panic!(
            "cannot start the Moto test container: {e}\n\
             Rootless podman: set DOCKER_HOST=unix://$XDG_RUNTIME_DIR/podman/podman.sock\n\
             Or skip containers and point {URL_VAR} at a running server."
        )
    });
    let port = c
        .get_host_port_ipv4(MOTO_PORT)
        .await
        .expect("no mapped Moto port");
    wait_ready(port).await;
    (format!("http://127.0.0.1:{port}"), c, permit)
}

/// Polls `GET /moto-api/` every 100 ms until it answers 200, for up to a minute.
async fn wait_ready(port: u16) {
    let deadline = tokio::time::Instant::now() + READY_TIMEOUT;
    while tokio::time::Instant::now() < deadline {
        if answers_200(port).await {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("the Moto test container did not answer on port {port} within a minute");
}

async fn answers_200(port: u16) -> bool {
    let Ok(mut stream) = tokio::net::TcpStream::connect(("127.0.0.1", port)).await else {
        return false;
    };
    if stream
        .write_all(b"GET /moto-api/ HTTP/1.0\r\n\r\n")
        .await
        .is_err()
    {
        return false;
    }
    let mut head = [0u8; 12];
    stream.read_exact(&mut head).await.is_ok() && head.ends_with(b" 200")
}
