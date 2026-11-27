//! DynamoDB harness: every `DynamoDb` is a seeded region leased on a capped Moto container or on
//! the server `DBENTO_DYNAMO_URL` names. Moto keeps tables per region, and a lease is a lock file
//! per server and region, so leases never collide, not even between processes.

use std::collections::{BTreeSet, HashMap};
use std::fs::{File, OpenOptions, TryLockError};
use std::path::Path;
use std::sync::{Mutex, OnceLock};
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

use crate::moto::{Moto, dy_source, moto};

/// The environment variable the generated connection reads its secret key from.
pub const DY_SECRET_VAR: &str = "DBENTO_SECRET_TEST_DY";
/// The access key and secret every handle signs with; Moto accepts any pair.
pub const DY_ACCESS_KEY: &str = "AKIDDBENTOTEST";
/// The secret key paired with `DY_ACCESS_KEY`; set it in `DY_SECRET_VAR` for a connection.
pub const DY_SECRET: &str = "dbento-moto-secret";
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

/// How long a handle waits for a region while every one is leased.
const LEASE_TIMEOUT: Duration = Duration::from_secs(300);
/// BatchWriteItem calls per chunk before a seed that keeps coming back unprocessed gives up.
const SEED_ATTEMPTS: u32 = 10;

/// Takes the lowest region index below `REGIONS.len()` missing from `taken`.
pub fn lease_from(taken: &mut BTreeSet<usize>) -> Option<usize> {
    let free = (0..REGIONS.len()).find(|i| !taken.contains(i))?;
    taken.insert(free);
    Some(free)
}

/// The regions this process holds, per sanitized endpoint, so endpoints never share a set.
fn leases() -> &'static Mutex<HashMap<String, BTreeSet<usize>>> {
    static LEASES: OnceLock<Mutex<HashMap<String, BTreeSet<usize>>>> = OnceLock::new();
    LEASES.get_or_init(Mutex::default)
}

/// A region held for one endpoint across every process on the machine, given back when dropped,
/// even by a handle that failed to seed.
pub struct RegionLease {
    server: String,
    index: usize,
    // An exclusive flock on the region's lock file; the kernel drops it when a process dies.
    _lock: File,
}

impl RegionLease {
    /// Takes the lowest region free for `endpoint` in this process and in every other, waiting
    /// up to `LEASE_TIMEOUT` while all are held.
    pub async fn take(endpoint: &str) -> Self {
        let dir = std::env::temp_dir().join("dbento-testkit-dynamo");
        std::fs::create_dir_all(&dir)
            .unwrap_or_else(|e| panic!("cannot create {}: {e}", dir.display()));
        let server: String = endpoint
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
            .collect();
        let deadline = tokio::time::Instant::now() + LEASE_TIMEOUT;
        loop {
            if let Some(lease) = Self::try_take(&dir, &server) {
                return lease;
            }
            if tokio::time::Instant::now() >= deadline {
                panic!("all 16 DynamoDB test regions are leased");
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// Walks the regions this process does not hold, lowest first, and keeps the first whose
    /// lock file no other process holds.
    fn try_take(dir: &Path, server: &str) -> Option<Self> {
        let mut all = leases().lock().unwrap_or_else(|p| p.into_inner());
        let taken = all.entry(server.to_owned()).or_default();
        let mut skipped = taken.clone();
        while let Some(index) = lease_from(&mut skipped) {
            let path = dir.join(format!("{server}-{}.lock", REGIONS[index]));
            let file = OpenOptions::new()
                .create(true)
                .truncate(false)
                .write(true)
                .open(&path)
                .unwrap_or_else(|e| panic!("cannot open {}: {e}", path.display()));
            match file.try_lock() {
                Ok(()) => {
                    taken.insert(index);
                    return Some(Self {
                        server: server.to_owned(),
                        index,
                        _lock: file,
                    });
                }
                Err(TryLockError::WouldBlock) => {}
                Err(TryLockError::Error(e)) => panic!("cannot lock {}: {e}", path.display()),
            }
        }
        None
    }

    /// The leased region's name.
    pub fn region(&self) -> &'static str {
        REGIONS[self.index]
    }
}

impl Drop for RegionLease {
    fn drop(&mut self) {
        let mut all = leases().lock().unwrap_or_else(|p| p.into_inner());
        if let Some(taken) = all.get_mut(&self.server) {
            taken.remove(&self.index);
            if taken.is_empty() {
                all.remove(&self.server);
            }
        }
    }
}

/// A seeded region. One holding a container must be dropped inside a tokio runtime.
pub struct DynamoDb {
    client: Client,
    // Declared before the lease, so a container is gone before its region is given back.
    server: Moto,
    lease: RegionLease,
}

impl DynamoDb {
    /// The server's `http://host:port`.
    pub fn endpoint(&self) -> &str {
        self.server.endpoint()
    }
    /// The leased region the seed lives in.
    pub fn region(&self) -> &str {
        self.lease.region()
    }
    /// A native client signed for this handle's region.
    pub fn client(&self) -> &Client {
        &self.client
    }

    /// `[connections.<name>]`: engine, region, endpoint, access_key_id,
    /// `secret = { env = DY_SECRET_VAR }`, then `extra` on its own line.
    pub fn connection_toml(&self, name: &str, extra: &str) -> String {
        let (region, endpoint) = (self.region(), self.endpoint());
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
        // ListTables answers at most 100 names a page, so follow its paging to the end.
        let mut start = None;
        loop {
            let listed = c
                .list_tables()
                .set_exclusive_start_table_name(start)
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
            start = listed.last_evaluated_table_name;
            if start.is_none() {
                break;
            }
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
    /// anything the server leaves unprocessed a bounded number of times.
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
            for attempt in 1.. {
                if pending.is_empty() {
                    break;
                }
                if attempt > SEED_ATTEMPTS {
                    panic!("Moto left orders items unprocessed after {SEED_ATTEMPTS} attempts");
                }
                if attempt > 1 {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
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
    let server = moto(dy_source(|k| std::env::var(k).ok())).await;
    let lease = RegionLease::take(server.endpoint()).await;
    let db = DynamoDb {
        client: client(server.endpoint(), lease.region()),
        server,
        lease,
    };
    db.seed().await;
    db
}
